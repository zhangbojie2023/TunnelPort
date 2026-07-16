// SSH tunnel implementation details
// The main SSH process lifecycle is now in tunnel/manager.rs
// This file contains helper functions for SSH key management and connection testing

use anyhow::{Result, anyhow};
use tokio::process::Command;

pub async fn check_ssh_connection(host: &str, port: u16) -> Result<()> {
    let output = Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", "StrictHostKeyChecking=accept-new",
            "-p", &port.to_string(),
            &format!("{}@{}", "test", host),
            "true",
        ])
        .output()
        .await
        .map_err(|e| anyhow!("ssh check failed: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!("SSH connection check failed"))
    }
}
