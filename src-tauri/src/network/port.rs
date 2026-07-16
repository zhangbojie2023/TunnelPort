use std::time::Duration;
use tokio::net::TcpStream;

pub async fn is_port_listening(port: u16) -> bool {
    let result = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .is_ok();
    tracing::debug!("Port {} listening: {}", port, result);
    result
}

pub async fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if is_port_listening(port).await {
            tracing::info!("Port {} ready (waited {:?})", port, start.elapsed());
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    tracing::warn!("Port {} timeout after {:?}", port, timeout);
    false
}
