use tauri::{Emitter, State};
use tauri_plugin_store::StoreExt;
use crate::state::{AppState, TunnelStatus};
use crate::config::{TunnelProfile, AppConfig};
use crate::network::reachability::HealthReport;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "macos")]
use crate::platform::macos as platform;

#[cfg(target_os = "windows")]
use crate::platform::windows as platform;

#[derive(serde::Serialize, Clone)]
pub struct StatusReport {
    pub status: String,
    pub socks_port_open: bool,
    pub http_proxy_open: bool,
    pub host_reachable: bool,
    pub latency_ms: Option<u64>,
    pub uptime_secs: Option<u64>,
    pub error_detail: Option<String>,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

/// Helper: get the active profile from app config
fn active_profile(app_config: &AppConfig) -> Result<TunnelProfile, String> {
    app_config
        .active_profile()
        .cloned()
        .ok_or_else(|| "No active profile selected".to_string())
}

/// Helper: persist app config to store
fn persist_config(app: &tauri::AppHandle, app_config: &AppConfig) -> Result<(), String> {
    let store = app.store("config.json").map_err(|e| {
        tracing::error!("Failed to open store: {}", e);
        e.to_string()
    })?;
    let json_val = serde_json::to_value(app_config).map_err(|e| {
        tracing::error!("Failed to serialize config: {}", e);
        e.to_string()
    })?;
    store.set("app_config", json_val);
    store.save().map_err(|e| {
        tracing::error!("Failed to save store: {}", e);
        e.to_string()
    })?;
    tracing::debug!("Config persisted to store");
    Ok(())
}

// ── Tunnel Commands ──

/// Stop and remove any running tunnel from state (no-op if none).
async fn cleanup_tunnel(state: &AppState) {
    let mut tunnel_lock = state.tunnel.lock().await;
    if let Some(mut t) = tunnel_lock.take() {
        tracing::info!("Cleaning up existing tunnel before new operation");
        let _ = t.stop().await;
    }
}

#[tauri::command]
pub async fn tunnel_start(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let app_config = state.app_config.read().await.clone();
    let profile = active_profile(&app_config)?;
    tracing::info!("Starting tunnel — profile: {:?}, host: {}:{}, user: {}",
        profile.name, profile.target_host, profile.ssh_port, profile.target_user);

    // Stop any existing tunnel before starting a new one
    cleanup_tunnel(&state).await;

    *state.status.write().await = TunnelStatus::Connecting;
    let _ = app.emit("tunnel-status", "connecting").ok();

    // Reset traffic stats for fresh connection
    state.stats.reset();

    match crate::tunnel::manager::ManagedTunnel::start(&profile, Arc::clone(&state.stats)).await {
        Ok(tunnel) => {
            *state.tunnel.lock().await = Some(tunnel);
            tracing::info!("Tunnel established — SOCKS :{}, HTTP :{}. Running connectivity tests...",
                profile.socks_port, profile.http_proxy_port);

            let test_url = profile.test_url.as_deref().unwrap_or("https://www.google.com");

            // Test 1: SOCKS5 proxy
            let _ = app.emit("tunnel-test-progress", serde_json::json!({ "step": "socks", "status": "testing" }));
            if let Err(e) = test_socks5_proxy(profile.socks_port, test_url).await {
                tracing::error!("SOCKS5 test failed: {}", e);
                let _ = app.emit("tunnel-test-progress", serde_json::json!({ "step": "socks", "status": "fail", "error": e }));
                cleanup_tunnel(&state).await;
                *state.status.write().await = TunnelStatus::Error(format!("SOCKS5 test failed: {}", e));
                let _ = app.emit("tunnel-status", "error").ok();
                return Err(format!("SOCKS5 proxy test failed: {}", e));
            }
            let _ = app.emit("tunnel-test-progress", serde_json::json!({ "step": "socks", "status": "ok" }));

            // Test 2: HTTP proxy
            let _ = app.emit("tunnel-test-progress", serde_json::json!({ "step": "http", "status": "testing" }));
            if let Err(e) = test_http_proxy(profile.http_proxy_port, test_url).await {
                tracing::error!("HTTP proxy test failed: {}", e);
                let _ = app.emit("tunnel-test-progress", serde_json::json!({ "step": "http", "status": "fail", "error": e }));
                cleanup_tunnel(&state).await;
                *state.status.write().await = TunnelStatus::Error(format!("HTTP proxy test failed: {}", e));
                let _ = app.emit("tunnel-status", "error").ok();
                return Err(format!("HTTP proxy test failed: {}", e));
            }
            let _ = app.emit("tunnel-test-progress", serde_json::json!({ "step": "http", "status": "ok" }));

            // Test 3: Target host reachability
            let _ = app.emit("tunnel-test-progress", serde_json::json!({ "step": "target", "status": "testing" }));
            if let Err(e) = test_target_reachable(&profile.target_host, profile.ssh_port).await {
                tracing::error!("Target reachability test failed: {}", e);
                let _ = app.emit("tunnel-test-progress", serde_json::json!({ "step": "target", "status": "fail", "error": e }));
                cleanup_tunnel(&state).await;
                *state.status.write().await = TunnelStatus::Error(format!("Target test failed: {}", e));
                let _ = app.emit("tunnel-status", "error").ok();
                return Err(format!("Target reachability test failed: {}", e));
            }
            let _ = app.emit("tunnel-test-progress", serde_json::json!({ "step": "target", "status": "ok" }));

            // All tests passed
            *state.status.write().await = TunnelStatus::Connected;
            *state.connected_at.write().await = Some(std::time::Instant::now());
            let _ = app.emit("tunnel-status", "connected").ok();
            tracing::info!("All connectivity tests passed — tunnel fully operational");

            if profile.auto_proxy {
                if let Err(e) = platform::set_system_proxy("127.0.0.1", profile.http_proxy_port).await {
                    tracing::warn!("Failed to set system proxy: {}", e);
                }
            }
            Ok(())
        }
        Err(e) => {
            tracing::error!("Tunnel start failed: {}", e);
            cleanup_tunnel(&state).await;
            *state.status.write().await = TunnelStatus::Error(e.to_string());
            let _ = app.emit("tunnel-status", "error").ok();
            Err(e.to_string())
        }
    }
}

#[tauri::command]
pub async fn tunnel_stop(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    tracing::info!("Stopping tunnel");

    let mut tunnel_lock = state.tunnel.lock().await;
    if let Some(tunnel) = tunnel_lock.as_mut() {
        if let Err(e) = tunnel.stop().await {
            tracing::warn!("Tunnel stop error: {}", e);
        }
    }
    *tunnel_lock = None;
    drop(tunnel_lock);

    if let Err(e) = platform::unset_system_proxy().await {
        tracing::warn!("Failed to unset system proxy: {}", e);
    }
    *state.status.write().await = TunnelStatus::Stopped;
    *state.connected_at.write().await = None;
    let _ = app.emit("tunnel-status", "stopped").ok();
    tracing::info!("Tunnel stopped");

    Ok(())
}

#[tauri::command]
pub async fn tunnel_status(state: State<'_, AppState>) -> Result<StatusReport, String> {
    let status = state.status.read().await.clone();
    let app_config = state.app_config.read().await.clone();
    let profile = active_profile(&app_config)?;

    // Use full reachability check to get latency
    let report = crate::network::reachability::check_reachability(
        &profile.target_host,
        profile.ssh_port,
        profile.socks_port,
        profile.http_proxy_port,
    ).await;

    // Calculate uptime
    let uptime_secs = {
        let connected_at = state.connected_at.read().await;
        connected_at.map(|t| t.elapsed().as_secs())
    };

    // Extract error detail and status string
    let (status_str, error_detail) = match &status {
        TunnelStatus::Stopped => ("stopped".to_string(), None),
        TunnelStatus::Connecting => ("connecting".to_string(), None),
        TunnelStatus::Connected => ("connected".to_string(), None),
        TunnelStatus::Reconnecting => ("reconnecting".to_string(), None),
        TunnelStatus::Error(e) => ("error".to_string(), Some(e.clone())),
    };

    Ok(StatusReport {
        status: status_str,
        socks_port_open: report.socks_port_open,
        http_proxy_open: report.http_proxy_open,
        host_reachable: report.host_reachable,
        latency_ms: report.latency_ms,
        uptime_secs,
        error_detail,
        bytes_up: state.stats.bytes_up.load(std::sync::atomic::Ordering::Relaxed),
        bytes_down: state.stats.bytes_down.load(std::sync::atomic::Ordering::Relaxed),
    })
}

#[tauri::command]
pub async fn proxy_toggle(
    state: State<'_, AppState>,
    enable: bool,
) -> Result<(), String> {
    let app_config = state.app_config.read().await.clone();
    let profile = active_profile(&app_config)?;
    tracing::info!("Proxy toggle: {} (port {})", if enable { "ON" } else { "OFF" }, profile.http_proxy_port);
    if enable {
        platform::set_system_proxy("127.0.0.1", profile.http_proxy_port).await
            .map_err(|e| { tracing::error!("Set proxy failed: {}", e); e.to_string() })
    } else {
        platform::unset_system_proxy().await
            .map_err(|e| { tracing::error!("Unset proxy failed: {}", e); e.to_string() })
    }
}

#[tauri::command]
pub async fn set_autostart(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<bool, String> {
    tracing::info!("Set autostart: {}", enabled);
    if enabled {
        platform::enable_autostart().await.map_err(|e| {
            tracing::error!("Enable autostart failed: {}", e);
            e.to_string()
        })?;
    } else {
        platform::disable_autostart().await.map_err(|e| {
            tracing::error!("Disable autostart failed: {}", e);
            e.to_string()
        })?;
    }

    let mut app_config = state.app_config.write().await;
    app_config.auto_start = enabled;
    persist_config(&app, &app_config)?;

    Ok(enabled)
}

#[tauri::command]
pub async fn test_connectivity(state: State<'_, AppState>) -> Result<HealthReport, String> {
    let app_config = state.app_config.read().await.clone();
    let profile = active_profile(&app_config)?;
    tracing::debug!("Testing connectivity for profile {:?}", profile.name);
    let report = crate::network::reachability::check_reachability(
        &profile.target_host,
        profile.ssh_port,
        profile.socks_port,
        profile.http_proxy_port,
    ).await;
    tracing::info!("Connectivity result: socks={}, http={}, host={}",
        report.socks_port_open, report.http_proxy_open, report.host_reachable);
    Ok(report)
}

#[tauri::command]
pub async fn test_proxy_connectivity(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let app_config = state.app_config.read().await.clone();
    let profile = active_profile(&app_config)?;
    let test_url = profile.test_url.as_deref().unwrap_or("https://www.google.com");

    let socks_result = test_socks5_proxy(profile.socks_port, test_url).await;
    let http_result = test_http_proxy(profile.http_proxy_port, test_url).await;
    let target_result = test_target_reachable(&profile.target_host, profile.ssh_port).await;

    Ok(serde_json::json!({
        "socks": {
            "pass": socks_result.is_ok(),
            "error": socks_result.err(),
            "url": test_url,
        },
        "http": {
            "pass": http_result.is_ok(),
            "error": http_result.err(),
            "url": test_url,
        },
        "target": {
            "pass": target_result.is_ok(),
            "error": target_result.err(),
            "host": format!("{}:{}", profile.target_host, profile.ssh_port),
        }
    }))
}

// ── App Settings Commands ──

#[tauri::command]
pub async fn get_auto_start(state: State<'_, AppState>) -> Result<bool, String> {
    let app_config = state.app_config.read().await;
    Ok(app_config.auto_start)
}

// ── Profile Management Commands ──

#[tauri::command]
pub async fn list_profiles(state: State<'_, AppState>) -> Result<Vec<TunnelProfile>, String> {
    let app_config = state.app_config.read().await;
    Ok(app_config.profiles.clone())
}

#[tauri::command]
pub async fn get_active_profile(state: State<'_, AppState>) -> Result<TunnelProfile, String> {
    let app_config = state.app_config.read().await.clone();
    active_profile(&app_config)
}

#[tauri::command]
pub async fn save_profile(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    profile: TunnelProfile,
) -> Result<(), String> {
    let mut app_config = state.app_config.write().await;

    // Update existing or add new
    if let Some(existing) = app_config.profiles.iter_mut().find(|p| p.id == profile.id) {
        tracing::info!("Updating profile: {:?} (id={})", profile.name, profile.id);
        *existing = profile;
    } else {
        tracing::info!("Creating profile: {:?} (id={})", profile.name, profile.id);
        // New profile — set as active if it's the first, or keep current active
        if app_config.profiles.is_empty() {
            app_config.active_profile_id = Some(profile.id.clone());
        }
        app_config.profiles.push(profile);
    }

    persist_config(&app, &app_config)?;
    Ok(())
}

#[tauri::command]
pub async fn delete_profile(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<(), String> {
    tracing::info!("Deleting profile: id={}", profile_id);
    let mut app_config = state.app_config.write().await;

    app_config.profiles.retain(|p| p.id != profile_id);

    // If we deleted the active profile, switch to the first remaining one
    if app_config.active_profile_id.as_deref() == Some(&profile_id) {
        app_config.active_profile_id = app_config.profiles.first().map(|p| p.id.clone());
        tracing::info!("Active profile changed to: {:?}", app_config.active_profile_id);
    }

    // Ensure at least one profile exists
    if app_config.profiles.is_empty() {
        let new_profile = TunnelProfile::new("Default");
        tracing::info!("No profiles left, creating default profile");
        app_config.active_profile_id = Some(new_profile.id.clone());
        app_config.profiles.push(new_profile);
    }

    persist_config(&app, &app_config)?;
    Ok(())
}

#[tauri::command]
pub async fn set_active_profile(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<(), String> {
    let mut app_config = state.app_config.write().await;

    // Verify profile exists
    if !app_config.profiles.iter().any(|p| p.id == profile_id) {
        tracing::error!("set_active_profile: profile not found: {}", profile_id);
        return Err("Profile not found".to_string());
    }

    let profile_name = app_config.profiles.iter()
        .find(|p| p.id == profile_id)
        .map(|p| p.name.clone());
    tracing::info!("Switching active profile to: {:?} (id={})", profile_name, profile_id);
    app_config.active_profile_id = Some(profile_id);
    persist_config(&app, &app_config)?;
    Ok(())
}

// ── Frontend Error Logging ──

#[tauri::command]
pub async fn get_local_ip() -> Result<String, String> {
    local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .map_err(|e| format!("Failed to get local IP: {}", e))
}

#[tauri::command]
pub async fn log_frontend_error(message: String, level: Option<String>) {
    let level = level.unwrap_or_else(|| "error".to_string());
    match level.as_str() {
        "error" => tracing::error!("[frontend] {}", message),
        "warn"  => tracing::warn!("[frontend] {}", message),
        _       => tracing::info!("[frontend] {}", message),
    }
}

// ── Native Confirm Dialog ──

#[tauri::command]
pub async fn confirm_dialog(app: tauri::AppHandle, message: String) -> Result<bool, String> {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let result = app
        .dialog()
        .message(&message)
        .title("PassPort")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Delete".into(),
            "Cancel".into(),
        ))
        .blocking_show();
    Ok(result)
}

// ── Log Viewer ──

#[tauri::command]
pub async fn get_recent_logs(lines: Option<usize>) -> Result<Vec<String>, String> {
    let lines = lines.unwrap_or(200);
    let log_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("passport")
        .join("logs");

    // Find today's log file
    let today = {
        use std::time::SystemTime;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let days = now.as_secs() / 86400;
        // Calculate date from unix days (simplified)
        let date = chrono_date(days);
        format!("passport.log.{}", date)
    };

    let log_path = log_dir.join(&today);
    if !log_path.exists() {
        // Try to find the most recent log file
        if let Ok(entries) = std::fs::read_dir(&log_dir) {
            let mut log_files: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("passport.log."))
                .collect();
            log_files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
            if let Some(latest) = log_files.first() {
                return read_last_lines(&latest.path(), lines);
            }
        }
        return Ok(vec!["No log files found".to_string()]);
    }

    read_last_lines(&log_path, lines)
}

fn read_last_lines(path: &std::path::Path, n: usize) -> Result<Vec<String>, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let all_lines: Vec<&str> = content.lines().collect();
    let start = all_lines.len().saturating_sub(n);
    Ok(all_lines[start..].iter().map(|l| l.to_string()).collect())
}

/// Simple date formatter from unix days (no chrono dependency).
fn chrono_date(days_since_epoch: u64) -> String {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days_since_epoch + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

// ── Post-connect connectivity tests ──

/// Parse a URL into (host, port). Defaults to port 443 for https, 80 for http.
fn parse_test_url(url: &str) -> Option<(String, u16)> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let (without_scheme, default_port) = if let Some(rest) = url.strip_prefix("https://") {
        (rest, 443u16)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (rest, 80u16)
    } else {
        (url, 443u16)
    };

    let host_port = without_scheme.split('/').next().unwrap_or(without_scheme);

    let (host, port) = if let Some((h, p)) = host_port.rsplit_once(':') {
        (h.to_string(), p.parse::<u16>().unwrap_or(default_port))
    } else {
        (host_port.to_string(), default_port)
    };

    if host.is_empty() {
        None
    } else {
        Some((host, port))
    }
}

/// Test SOCKS5 proxy: first do a fast greeting handshake to verify the tunnel
/// is alive, then optionally try a CONNECT to the test target (may time out on
/// slow networks — timeout is just a warning, not a hard failure).
async fn test_socks5_proxy(socks_port: u16, test_url: &str) -> Result<(), String> {
    let (host, port) = parse_test_url(test_url)
        .ok_or_else(|| "Invalid test URL".to_string())?;

    // Phase 1: greeting handshake (fast — verifies tunnel is alive)
    let handshake = async {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", socks_port))
            .await
            .map_err(|e| format!("Cannot connect to SOCKS5 on port {}: {}", socks_port, e))?;

        stream.write_all(&[0x05, 0x01, 0x00]).await
            .map_err(|e| format!("SOCKS5 greeting write: {}", e))?;
        let mut buf = [0u8; 2];
        stream.read_exact(&mut buf).await
            .map_err(|e| format!("SOCKS5 greeting read: {}", e))?;
        if buf[0] != 0x05 || buf[1] == 0xFF {
            return Err("SOCKS5 auth negotiation failed".to_string());
        }

        tracing::info!("SOCKS5 greeting OK on port {}", socks_port);
        Ok::<_, String>(stream)
    };

    let mut stream = timeout(Duration::from_secs(10), handshake)
        .await
        .map_err(|_| "SOCKS5 greeting timed out (10s)".to_string())??;

    // Phase 2: CONNECT to test target (may time out on slow networks)
    let connect = async {
        let domain = host.as_bytes();
        let mut req = vec![0x05, 0x01, 0x00, 0x03, domain.len() as u8];
        req.extend_from_slice(domain);
        req.extend_from_slice(&port.to_be_bytes());
        stream.write_all(&req).await
            .map_err(|e| format!("SOCKS5 CONNECT write: {}", e))?;

        let mut resp = [0u8; 4];
        stream.read_exact(&mut resp).await
            .map_err(|e| format!("SOCKS5 CONNECT response read: {}", e))?;
        if resp[1] != 0x00 {
            return Err(format!("SOCKS5 CONNECT failed (code={})", resp[1]));
        }

        tracing::info!("SOCKS5 CONNECT passed -> {}:{}", host, port);
        Ok(())
    };

    match timeout(Duration::from_secs(30), connect).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => {
            // CONNECT timed out but greeting passed — tunnel is alive, just
            // the target may be slow/unreachable. Warn but don't fail.
            tracing::warn!(
                "SOCKS5 CONNECT to {}:{} timed out (30s), but greeting passed — tunnel is alive",
                host, port
            );
            Ok(())
        }
    }
}

/// Test HTTP proxy by sending a CONNECT request.
async fn test_http_proxy(http_port: u16, test_url: &str) -> Result<(), String> {
    let (host, port) = parse_test_url(test_url)
        .ok_or_else(|| "Invalid test URL".to_string())?;

    let test_future = async {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", http_port))
            .await
            .map_err(|e| format!("Cannot connect to HTTP proxy on port {}: {}", http_port, e))?;

        let connect_req = format!(
            "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\n\r\n",
            host, port, host, port
        );
        stream.write_all(connect_req.as_bytes()).await
            .map_err(|e| format!("HTTP CONNECT write: {}", e))?;

        // Read response line
        let mut buf = vec![0u8; 1024];
        let n = stream.read(&mut buf).await
            .map_err(|e| format!("HTTP CONNECT read: {}", e))?;
        let response = String::from_utf8_lossy(&buf[..n]);

        if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
            return Err(format!("HTTP CONNECT failed: {}", response.lines().next().unwrap_or("?")));
        }

        tracing::info!("HTTP proxy test passed -> {}:{}", host, port);
        Ok(())
    };

    timeout(Duration::from_secs(10), test_future)
        .await
        .map_err(|_| "HTTP proxy test timed out (10s)".to_string())?
}

/// Test TCP reachability to target host.
async fn test_target_reachable(host: &str, port: u16) -> Result<(), String> {
    let test_future = async {
        TcpStream::connect(format!("{}:{}", host, port))
            .await
            .map_err(|e| format!("Target {}:{} unreachable: {}", host, port, e))?;
        tracing::info!("Target reachability test passed -> {}:{}", host, port);
        Ok(())
    };

    timeout(Duration::from_secs(10), test_future)
        .await
        .map_err(|_| format!("Target {}:{} reachability test timed out (10s)", host, port))?
}
