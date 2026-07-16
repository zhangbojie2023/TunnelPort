use serde::Serialize;
use tokio::sync::{Mutex, RwLock};
use std::sync::Arc;
use std::time::Instant;
use crate::config::AppConfig;
use crate::stats::ProxyStats;
use crate::tunnel::manager::ManagedTunnel;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub enum TunnelStatus {
    Stopped,
    Connecting,
    Connected,
    Reconnecting,
    Error(String),
}

pub struct AppState {
    pub app_config: Arc<RwLock<AppConfig>>,
    pub tunnel: Arc<Mutex<Option<ManagedTunnel>>>,
    pub status: Arc<RwLock<TunnelStatus>>,
    pub connected_at: Arc<RwLock<Option<Instant>>>,
    pub stats: Arc<ProxyStats>,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        Self {
            app_config: Arc::new(RwLock::new(config)),
            tunnel: Arc::new(Mutex::new(None)),
            status: Arc::new(RwLock::new(TunnelStatus::Stopped)),
            connected_at: Arc::new(RwLock::new(None)),
            stats: Arc::new(ProxyStats::default()),
        }
    }
}
