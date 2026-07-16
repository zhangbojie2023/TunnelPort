use serde::Serialize;
use tokio::net::TcpStream;
use std::time::Instant;
use crate::network::port;

#[derive(Serialize, Clone, Debug)]
pub struct HealthReport {
    pub socks_port_open: bool,
    pub http_proxy_open: bool,
    pub host_reachable: bool,
    pub ssh_port_open: bool,
    pub latency_ms: Option<u64>,
}

pub async fn check_reachability(
    host: &str,
    ssh_port: u16,
    socks_port: u16,
    http_port: u16,
) -> HealthReport {
    let socks_open = port::is_port_listening(socks_port).await;
    let http_open = port::is_port_listening(http_port).await;
    let (host_reachable, latency_ms) = check_host_reachable_with_latency(host, ssh_port).await;

    let report = HealthReport {
        socks_port_open: socks_open,
        http_proxy_open: http_open,
        host_reachable,
        ssh_port_open: host_reachable,
        latency_ms,
    };

    tracing::debug!("Reachability [{host}:{ssh_port}]: socks={socks_open}, http={http_open}, host={host_reachable}, latency={latency_ms:?}ms");
    report
}

pub async fn check_host_reachable(host: &str, port: u16) -> bool {
    let (result, _) = check_host_reachable_with_latency(host, port).await;
    result
}

async fn check_host_reachable_with_latency(host: &str, port: u16) -> (bool, Option<u64>) {
    let start = Instant::now();
    let result = TcpStream::connect(format!("{}:{}", host, port))
        .await
        .is_ok();
    let latency = if result {
        Some(start.elapsed().as_millis() as u64)
    } else {
        None
    };
    tracing::debug!("Host {}:{} reachable: {} ({}ms)", host, port, result, latency.unwrap_or(0));
    (result, latency)
}
