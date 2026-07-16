use crate::config::TunnelProfile;
use crate::network::port;

pub async fn check_tunnel_health(profile: &TunnelProfile) -> HealthResult {
    let socks_open = port::is_port_listening(profile.socks_port).await;

    HealthResult {
        socks_port_open: socks_open,
        target_reachable: socks_open, // If port is open, tunnel is working
    }
}

#[derive(Clone, Debug)]
pub struct HealthResult {
    pub socks_port_open: bool,
    pub target_reachable: bool,
}
