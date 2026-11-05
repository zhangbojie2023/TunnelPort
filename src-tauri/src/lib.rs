mod config;
mod state;
mod commands;
mod tunnel;
mod proxy;
mod platform;
mod network;
mod events;
mod log_layer;
mod stats;

#[cfg(target_os = "macos")]
use platform::macos as platform_impl;
#[cfg(target_os = "windows")]
use platform::windows as platform_impl;

use std::sync::Arc;
use tokio::sync::RwLock;
use tauri::{Emitter, Manager};
use tauri_plugin_store::StoreExt;
use tauri::tray::TrayIconBuilder;
use tauri::menu::{Menu, MenuItem};
use tracing_subscriber::prelude::*;

// Tray menu item IDs
const TRAY_CONNECT: &str = "tray_connect";
const TRAY_DISCONNECT: &str = "tray_disconnect";
const TRAY_SHOW: &str = "tray_show";
const TRAY_QUIT: &str = "tray_quit";

use config::AppConfig;
use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // macOS GUI apps launched from Finder/Dock don't inherit the shell's PATH.
    // Add Homebrew paths so `which::which` can find sshpass, gost, etc.
    #[cfg(target_os = "macos")]
    {
        let homebrew_paths = ["/opt/homebrew/bin", "/usr/local/bin"];
        let current_path = std::env::var("PATH").unwrap_or_default();
        let mut new_paths: Vec<&str> = Vec::new();
        for p in &homebrew_paths {
            if !current_path.contains(p) {
                new_paths.push(*p);
            }
        }
        if !new_paths.is_empty() {
            new_paths.push(&current_path);
            std::env::set_var("PATH", new_paths.join(":"));
        }
    }

    // Set up logging: stderr + daily rotating file + Tauri event forwarding
    let log_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("passport")
        .join("logs");
    std::fs::create_dir_all(&log_dir).ok();

    let file_appender = tracing_appender::rolling::daily(&log_dir, "passport.log");
    let (event_log_layer, event_log_handle) = log_layer::EventLogLayer::new();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("passport=debug".parse().unwrap()),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(tracing_subscriber::fmt::layer().with_writer(file_appender))
        .with(event_log_layer)
        .init();

    tracing::info!("Logging initialized — log dir: {}", log_dir.display());

    let config = AppConfig::default();
    let app_state = AppState::new(config);

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            tracing::debug!("Setup: loading saved config from store");

            // Inject AppHandle into event log layer for real-time log forwarding
            *event_log_handle.lock().unwrap() = Some(app.handle().clone());

            // Diagnostic: list all webview windows
            let windows = app.webview_windows();
            tracing::info!("Setup: found {} webview window(s)", windows.len());
            for (label, _window) in &windows {
                tracing::info!("Setup: webview window label = {:?}", label);
            }

            // Load saved config from store (if exists)
            if let Ok(store) = app.store("config.json") {
                if let Some(val) = store.get("app_config") {
                    match serde_json::from_value::<AppConfig>(val) {
                        Ok(cfg) => {
                            let state = app.state::<AppState>();
                            *state.app_config.blocking_write() = cfg;
                            tracing::debug!("Setup: loaded saved config from store");
                        }
                        Err(e) => tracing::warn!("Setup: failed to parse saved config: {}", e),
                    }
                }
            }

            tracing::debug!("Setup: building tray menu");

            // System tray with menu
            let connect_item = MenuItem::with_id(app, TRAY_CONNECT, "Connect", true, None::<&str>)?;
            let disconnect_item = MenuItem::with_id(app, TRAY_DISCONNECT, "Disconnect", false, None::<&str>)?;
            let show_item = MenuItem::with_id(app, TRAY_SHOW, "Show Panel", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, TRAY_QUIT, "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&connect_item, &disconnect_item, &show_item, &quit_item])?;

            let _app_handle = app.handle().clone();
            let icon = app.default_window_icon().cloned().unwrap_or_else(|| {
                tracing::warn!("No default window icon found, using 1x1 fallback");
                tauri::image::Image::new_owned(vec![0, 0, 0, 0], 1, 1)
            });
            tracing::debug!("Setup: building tray icon");
            let _tray = TrayIconBuilder::new()
                .icon(icon)
                .tooltip("PassPort")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(move |app, event| {
                    match event.id.as_ref() {
                        TRAY_CONNECT => {
                            let _ = app.emit("tray-connect", ());
                        }
                        TRAY_DISCONNECT => {
                            let _ = app.emit("tray-disconnect", ());
                        }
                        TRAY_SHOW => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        TRAY_QUIT => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            // Start health check loop
            tracing::debug!("Setup: starting health check loop");
            let state = app.state::<AppState>();
            let _handle = app.handle().clone();
            tauri::async_runtime::spawn(health_check_loop(
                _handle.clone(),
                Arc::clone(&state.status),
                Arc::clone(&state.app_config),
                Arc::clone(&state.stats),
            ));

            // Register window close handler — trigger cleanup before exit
            if let Some(window) = app.get_webview_window("main") {
                let tunnel_arc = Arc::clone(&_handle.state::<AppState>().tunnel);
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { .. } = event {
                        tracing::info!("Window close requested — stopping tunnel before exit");
                        let maybe_tunnel = tunnel_arc.try_lock().ok().and_then(|mut g| g.take());
                        if let Some(mut tunnel) = maybe_tunnel {
                            let _ = tauri::async_runtime::block_on(tunnel.stop());
                        }
                        let _ = tauri::async_runtime::block_on(platform_impl::unset_system_proxy());
                    }
                });
            }

            tracing::debug!("Setup: complete");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::tunnel_start,
            commands::tunnel_stop,
            commands::tunnel_cancel,
            commands::tunnel_status,
            commands::proxy_toggle,
            commands::set_autostart,
            commands::get_auto_start,
            commands::test_connectivity,
            commands::test_proxy_connectivity,
            commands::list_profiles,
            commands::get_active_profile,
            commands::save_profile,
            commands::delete_profile,
            commands::set_active_profile,
            commands::log_frontend_error,
            commands::get_recent_logs,
            commands::confirm_dialog,
            commands::get_local_ip,
        ])
        .manage(app_state)
        .build(tauri::generate_context!())
        .expect("Tauri build failed — check setup() closure for panics")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                tracing::info!("App exiting — ensuring tunnel cleanup");
                let tunnel_arc = Arc::clone(&app_handle.state::<AppState>().tunnel);
                let maybe_tunnel = tunnel_arc.try_lock().ok().and_then(|mut g| g.take());
                if let Some(mut tunnel) = maybe_tunnel {
                    let _ = tauri::async_runtime::block_on(tunnel.stop());
                }
            }
        });
}

async fn health_check_loop(
    app_handle: tauri::AppHandle,
    status: Arc<RwLock<state::TunnelStatus>>,
    app_config: Arc<RwLock<AppConfig>>,
    proxy_stats: Arc<stats::ProxyStats>,
) {
    use tokio::time::{interval, Duration};
    let mut interval = interval(Duration::from_secs(10));
    loop {
        interval.tick().await;
        let cfg = app_config.read().await.clone();
        let current_status = status.read().await.clone();

        // Use active profile for health check
        let profile = match cfg.active_profile() {
            Some(p) => p.clone(),
            None => continue,
        };

        let report = network::reachability::check_reachability(
            &profile.target_host,
            profile.ssh_port,
            profile.socks_port,
            profile.http_proxy_port,
        )
        .await;

        // Merge traffic stats into health-check payload
        let (bytes_up, bytes_down) = proxy_stats.snapshot();
        let mut payload = serde_json::to_value(&report).unwrap_or_default();
        payload["bytes_up"] = serde_json::json!(bytes_up);
        payload["bytes_down"] = serde_json::json!(bytes_down);
        let _ = app_handle.emit("health-check", payload);

        // Auto-reconnect: if connected but health check fails
        if matches!(current_status, state::TunnelStatus::Connected)
            && (!report.socks_port_open || !report.host_reachable)
        {
            tracing::warn!("Health check failed, triggering reconnect");
            *status.write().await = state::TunnelStatus::Reconnecting;
            let _ = app_handle.emit("tunnel-status", "reconnecting");
            let _ = app_handle.emit("tunnel-error", "Tunnel health check failed, reconnecting...");
        }
    }
}
