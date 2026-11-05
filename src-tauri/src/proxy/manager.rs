use anyhow::Result;
use std::sync::Arc;
use crate::config::TunnelProfile;
use crate::proxy::bridge::ProxyBridge;
use crate::stats::ProxyStats;

/// Start proxy bridge. Uses Rust implementation by default, falls back to gost.
pub async fn start_proxy(config: &TunnelProfile, stats: Arc<ProxyStats>) -> Result<ProxyInstance> {
    // Try Rust bridge first
    match ProxyBridge::start(config.socks_port, config.http_proxy_port, stats).await {
        Ok(bridge) => {
            tracing::info!("Rust SOCKS5→HTTP proxy bridge started on :{}", config.http_proxy_port);
            return Ok(ProxyInstance::Rust(bridge));
        }
        Err(e) => {
            tracing::warn!("Rust bridge failed: {}, falling back to gost", e);
        }
    }

    // Fallback to gost
    if which::which("gost").is_ok() {
        tracing::info!("Starting gost proxy bridge (fallback)");
        let mut cmd = tokio::process::Command::new("gost");
        cmd.args([
            "-L",
            &format!("http://:{}", config.http_proxy_port),
            "-F",
            &format!("socks5://127.0.0.1:{}", config.socks_port),
        ]);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);  // Don't orphan gost if the connect is cancelled

        let child = cmd.spawn()
            .map_err(|e| anyhow::anyhow!("Failed to start gost: {}", e))?;

        crate::network::port::wait_for_port(
            config.http_proxy_port,
            std::time::Duration::from_secs(10),
        ).await;

        return Ok(ProxyInstance::Gost(child));
    }

    Err(anyhow::anyhow!(
        "No proxy bridge available. Install gost via: brew install gost"
    ))
}

pub enum ProxyInstance {
    Rust(ProxyBridge),
    Gost(tokio::process::Child),
}

impl ProxyInstance {
    pub async fn stop(&mut self) -> Result<()> {
        match self {
            ProxyInstance::Rust(bridge) => {
                bridge.abort().await;
                Ok(())
            }
            ProxyInstance::Gost(child) => {
                child.kill().await?;
                let _ = child.wait().await;
                Ok(())
            }
        }
    }
}
