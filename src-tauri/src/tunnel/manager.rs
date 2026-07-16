use anyhow::{Result, anyhow};
use std::sync::Arc;
use tokio::process::Child;
use crate::config::TunnelProfile;
use crate::network::port;
use crate::proxy::manager::ProxyInstance;
use crate::stats::ProxyStats;

pub struct ManagedTunnel {
    pub ssh_child: Child,
    pub proxy: Option<ProxyInstance>,
}

impl ManagedTunnel {
    pub async fn start(config: &TunnelProfile, stats: Arc<ProxyStats>) -> Result<Self> {
        tracing::info!("Starting tunnel: SSH to {}@{}:{}", config.target_user, config.target_host, config.ssh_port);

        // Start SSH tunnel
        let mut ssh_child = start_ssh_tunnel(config).await?;
        tracing::info!("SSH tunnel established on port {}", config.socks_port);

        // Start proxy bridge
        let proxy = match crate::proxy::manager::start_proxy(config, stats).await {
            Ok(proxy) => {
                // Verify SSH survived proxy startup
                if let Some(status) = ssh_child.try_wait().transpose() {
                    let stderr_text = read_child_stderr(&mut ssh_child).await;
                    let code = status.map(|s| s.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into()))
                        .unwrap_or_else(|_| "?".into());
                    tracing::error!("SSH died during proxy startup (code: {}): {}", code, stderr_text);
                    let _ = ssh_child.kill().await;
                    return Err(anyhow!("SSH tunnel died during proxy startup (code: {}): {}", code, stderr_text));
                }
                Some(proxy)
            }
            Err(e) => {
                tracing::warn!("Proxy bridge failed: {}", e);
                let _ = ssh_child.kill().await;
                return Err(e);
            }
        };

        Ok(ManagedTunnel { ssh_child, proxy })
    }

    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut proxy) = self.proxy.take() {
            let _ = proxy.stop().await;
            tracing::info!("Proxy bridge stopped");
        }

        match self.ssh_child.kill().await {
            Ok(_) => tracing::debug!("SSH process killed"),
            Err(e) => tracing::warn!("SSH process kill error: {}", e),
        }
        let status = self.ssh_child.wait().await;
        tracing::info!("SSH tunnel stopped (exit: {:?})", status.map(|s| s.code()));

        Ok(())
    }
}

/// Read stderr from an already-exited child process. Only call after try_wait()
/// returns Some — taking stderr while the process is alive closes the pipe and
/// causes SIGPIPE, killing the process.
async fn read_child_stderr(child: &mut Child) -> String {
    let mut stderr = match child.stderr.take() {
        Some(s) => s,
        None => return "(no stderr handle)".to_string(),
    };
    let mut buf = vec![0u8; 8192];
    match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::io::AsyncReadExt::read(&mut stderr, &mut buf),
    ).await {
        Ok(Ok(n)) if n > 0 => String::from_utf8_lossy(&buf[..n]).trim().to_string(),
        _ => "(no stderr output)".to_string(),
    }
}

async fn start_ssh_tunnel(config: &TunnelProfile) -> Result<Child> {
    let ssh_path = which::which("ssh").map_err(|e| anyhow!("ssh not found: {}", e))?;

    let auth_method = if config.password.is_some() { "password (sshpass)" } else { "key" };
    tracing::info!("SSH auth method: {}", auth_method);

    let mut cmd = if let Some(password) = config.password.as_ref() {
        // Use sshpass for password authentication
        let sshpass_path = which::which("sshpass")
            .map_err(|_| anyhow!("sshpass not found. Install via: brew install hudochenkov/sshpass/sshpass"))?;
        let mut c = tokio::process::Command::new(&sshpass_path);
        c.args(["-p", password]);
        c.arg(ssh_path.to_str().unwrap());
        c
    } else {
        tokio::process::Command::new(&ssh_path)
    };

    let ssh_args = config.ssh_args();
    tracing::debug!("SSH args: {:?}", ssh_args);
    cmd.args(&ssh_args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);  // Ensure SSH dies when ManagedTunnel is dropped

    let mut child = cmd.spawn().map_err(|e| {
        tracing::error!("Failed to spawn SSH: {}", e);
        anyhow!("Failed to start ssh: {}", e)
    })?;

    // Wait for SOCKS port to be ready, also monitor SSH process exit (timeout 30s)
    let timeout = std::time::Duration::from_secs(30);
    let start = std::time::Instant::now();
    let mut port_ready = false;

    while start.elapsed() < timeout {
        // Check if port is listening
        if port::is_port_listening(config.socks_port).await {
            port_ready = true;
            tracing::info!("Port {} ready (waited {:?})", config.socks_port, start.elapsed());
            break;
        }

        // Check if SSH process has exited prematurely (indicates a failure)
        match child.try_wait() {
            Ok(Some(status)) => {
                let stderr_text = read_child_stderr(&mut child).await;
                let exit_code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".to_string());
                tracing::error!("SSH process exited early (code: {}), stderr: {}", exit_code, stderr_text);
                let _ = child.kill().await;
                return Err(anyhow!(
                    "SSH process exited with code {}: {}",
                    exit_code,
                    stderr_text
                ));
            }
            Ok(None) => {} // Still running, continue waiting
            Err(e) => {
                tracing::warn!("Failed to check SSH process status: {}", e);
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    if !port_ready {
        // Final check: did process exit right after the loop?
        match child.try_wait() {
            Ok(Some(status)) => {
                let stderr_text = read_child_stderr(&mut child).await;
                let exit_code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".to_string());
                tracing::error!("SSH exited with code {}, stderr: {}", exit_code, stderr_text);
                let _ = child.kill().await;
                return Err(anyhow!(
                    "SSH process exited with code {}: {}",
                    exit_code,
                    stderr_text
                ));
            }
            _ => {}
        }

        tracing::error!(
            "SOCKS port {} did not become ready within {}s",
            config.socks_port,
            timeout.as_secs()
        );
        return Err(anyhow!(
            "SOCKS port {} did not become ready within {}s (SSH may still be connecting, port may be in use, or a firewall is blocking localhost:{})",
            config.socks_port,
            timeout.as_secs(),
            config.socks_port
        ));
    }

    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ssh_failure_surfaces_stderr() {
        let config = TunnelProfile {
            id: "test".into(),
            name: "test".into(),
            target_host: "bogus.nonexistent.example".into(),
            target_user: "nobody".into(),
            password: None,
            ssh_port: 22,
            socks_port: 29999,
            http_proxy_port: 29998,
            ssh_key_path: None,
            auto_proxy: false,
            auto_reconnect: false,
            test_url: None,
        };

        let result = start_ssh_tunnel(&config).await;
        assert!(result.is_err(), "Expected error for bogus host");

        let err = result.unwrap_err().to_string();
        eprintln!("Captured error: {}", err);

        // Must contain SSH's real error reason, not just the old generic message
        let has_ssh_error = err.contains("Could not resolve hostname")
            || err.contains("nodename nor servname")
            || err.contains("Connection refused")
            || err.contains("No route to host")
            || err.contains("Permission denied")
            || err.contains("connection closed");

        assert!(
            has_ssh_error,
            "Error should surface SSH stderr, got: {}",
            err
        );
    }

    /// Verify SSH stays alive after success — the stderr pipe must NOT be
    /// prematurely closed (the bug this test guards against).
    #[tokio::test]
    async fn ssh_survives_after_port_ready() {
        // Try localhost SSH — this test is opportunistic (passes if SSH is available)
        let config = TunnelProfile {
            id: "test".into(),
            name: "test".into(),
            target_host: "127.0.0.1".into(),
            target_user: std::env::var("USER").unwrap_or_else(|_| "nobody".into()),
            password: None,
            ssh_port: 22,
            socks_port: 29997,
            http_proxy_port: 29996,
            ssh_key_path: None,
            auto_proxy: false,
            auto_reconnect: false,
            test_url: None,
        };

        let result = start_ssh_tunnel(&config).await;
        match result {
            Ok(mut child) => {
                // SUCCESS: tunnel started. Verify it stays alive for 3 seconds.
                eprintln!("SSH tunnel started on port 29997 — verifying it stays alive...");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                match child.try_wait() {
                    Ok(None) => {
                        eprintln!("PASS: SSH still running after 3s (stderr pipe intact)");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                    Ok(Some(status)) => {
                        // Read stderr to diagnose WHY it died
                        let mut stderr_buf = vec![0u8; 4096];
                        let stderr = if let Some(mut s) = child.stderr.take() {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(2),
                                tokio::io::AsyncReadExt::read(&mut s, &mut stderr_buf),
                            ).await;
                            String::from_utf8_lossy(&stderr_buf).trim().to_string()
                        } else {
                            "(no stderr)".into()
                        };
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        panic!(
                            "SSH died within 3s of port becoming ready (exit: {:?}). stderr: {}",
                            status.code(),
                            if stderr.is_empty() { "(empty)" } else { &stderr }
                        );
                    }
                    Err(e) => {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        panic!("try_wait error: {}", e);
                    }
                }
            }
            Err(e) => {
                // SSH to localhost may fail if SSH server isn't running —
                // this is fine; the test is opportunistic.
                eprintln!("SSH to localhost failed (expected if no local SSH server): {}", e);
                // Still verify the error message contains real SSH stderr
                assert!(
                    e.to_string().contains("SSH process exited"),
                    "Error should contain SSH exit info, got: {}",
                    e
                );
            }
        }
    }
}
