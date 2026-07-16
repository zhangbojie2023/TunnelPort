use anyhow::{Result, anyhow};
use std::fs;
use std::path::PathBuf;

const PLIST_ID: &str = "com.zhangbojie.passport";

pub fn plist_path() -> PathBuf {
    let home = dirs::home_dir().expect("Could not find home directory");
    home.join("Library/LaunchAgents").join(format!("{}.plist", PLIST_ID))
}

pub async fn enable_autostart() -> Result<()> {
    let app_path = std::env::current_exe()
        .map_err(|e| anyhow!("Failed to get current exe path: {}", e))?;

    let plist = format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>"#, PLIST_ID, app_path.display());

    let path = plist_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| anyhow!("Failed to create LaunchAgents directory: {}", e))?;
    }
    fs::write(&path, plist)
        .map_err(|e| anyhow!("Failed to write plist: {}", e))?;

    tracing::info!("Autostart enabled at {}", path.display());
    Ok(())
}

pub async fn disable_autostart() -> Result<()> {
    let path = plist_path();
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| anyhow!("Failed to remove plist: {}", e))?;
        tracing::info!("Autostart disabled, removed {}", path.display());
    }
    Ok(())
}

pub async fn is_autostart_enabled() -> bool {
    plist_path().exists()
}

pub async fn set_system_proxy(host: &str, port: u16) -> Result<()> {
    tracing::info!("Setting system proxy to {}:{}", host, port);

    // Set web proxy
    match tokio::process::Command::new("networksetup")
        .args(["-setwebproxy", "Wi-Fi", host, &port.to_string()])
        .output()
        .await
    {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::error!("networksetup -setwebproxy failed: {}", stderr);
            }
        }
        Err(e) => tracing::error!("Failed to run networksetup -setwebproxy: {}", e),
    }

    // Set secure web proxy
    match tokio::process::Command::new("networksetup")
        .args(["-setsecurewebproxy", "Wi-Fi", host, &port.to_string()])
        .output()
        .await
    {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::error!("networksetup -setsecurewebproxy failed: {}", stderr);
            }
        }
        Err(e) => tracing::error!("Failed to run networksetup -setsecurewebproxy: {}", e),
    }

    Ok(())
}

pub async fn unset_system_proxy() -> Result<()> {
    tracing::info!("Unsetting system proxy");

    match tokio::process::Command::new("networksetup")
        .args(["-setwebproxystate", "Wi-Fi", "off"])
        .output()
        .await
    {
        Ok(output) if !output.status.success() => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!("networksetup -setwebproxystate failed: {}", stderr);
        }
        Err(e) => tracing::error!("Failed to run networksetup -setwebproxystate: {}", e),
        _ => {}
    }

    match tokio::process::Command::new("networksetup")
        .args(["-setsecurewebproxystate", "Wi-Fi", "off"])
        .output()
        .await
    {
        Ok(output) if !output.status.success() => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!("networksetup -setsecurewebproxystate failed: {}", stderr);
        }
        Err(e) => tracing::error!("Failed to run networksetup -setsecurewebproxystate: {}", e),
        _ => {}
    }

    Ok(())
}
