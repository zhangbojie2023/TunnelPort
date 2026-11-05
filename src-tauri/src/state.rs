use serde::Serialize;
use tokio::sync::{Mutex, RwLock, watch};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Broadcast cancellation for in-flight connect attempts.
///
/// The channel carries a monotonically increasing "cancel generation" instead
/// of a bool. `subscribe()` hands out a receiver whose current value is already
/// marked as seen, so `changed()` only fires for cancellations that happen
/// *after* that particular connect started. With a reusable bool, the stale
/// `true` from a previous attempt would instantly cancel the next one.
pub struct CancelState {
    sender: watch::Sender<u64>,
    /// A cancel that arrived before any connect was listening. Without this,
    /// clicking Cancel in the gap between the UI flipping to CONNECTING and
    /// `tunnel_start` subscribing would be silently lost.
    pending: AtomicBool,
}

impl CancelState {
    pub fn new() -> Self {
        let (sender, _) = watch::channel(0u64);
        Self { sender, pending: AtomicBool::new(false) }
    }

    /// Fresh receiver for one connect attempt, plus whether a cancel already
    /// landed before this subscription existed.
    pub fn subscribe(&self) -> (watch::Receiver<u64>, bool) {
        let rx = self.sender.subscribe();
        let already_cancelled = self.pending.swap(false, Ordering::AcqRel);
        (rx, already_cancelled)
    }

    /// Wake every await point of the current connect attempt.
    pub fn cancel(&self) {
        // No listener yet → leave a sticky flag for the connect about to start.
        // A live connect already has a receiver, so the generation bump alone
        // is enough and we must not leak a cancel into the *next* attempt.
        if self.sender.receiver_count() == 0 {
            self.pending.store(true, Ordering::Release);
        }
        self.sender.send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

impl Default for CancelState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AppState {
    pub app_config: Arc<RwLock<AppConfig>>,
    pub tunnel: Arc<Mutex<Option<ManagedTunnel>>>,
    pub status: Arc<RwLock<TunnelStatus>>,
    pub connected_at: Arc<RwLock<Option<Instant>>>,
    pub stats: Arc<ProxyStats>,
    pub cancel: CancelState,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        Self {
            app_config: Arc::new(RwLock::new(config)),
            tunnel: Arc::new(Mutex::new(None)),
            status: Arc::new(RwLock::new(TunnelStatus::Stopped)),
            connected_at: Arc::new(RwLock::new(None)),
            stats: Arc::new(ProxyStats::default()),
            cancel: CancelState::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CancelState;

    #[tokio::test]
    async fn a_cancel_does_not_leak_into_the_next_attempt() {
        let cancel = CancelState::new();

        let (mut first, pending) = cancel.subscribe();
        assert!(!pending);
        cancel.cancel();
        assert!(first.changed().await.is_ok(), "the running connect must be woken");

        // A fresh subscription starts at the current generation and must not
        // inherit the stale cancel from the previous attempt.
        let (mut second, pending) = cancel.subscribe();
        assert!(!pending);
        assert!(!second.has_changed().unwrap());

        // ...but it is still cancellable on its own.
        cancel.cancel();
        assert!(second.changed().await.is_ok());
    }

    #[tokio::test]
    async fn a_cancel_fired_before_subscribe_is_not_lost() {
        let cancel = CancelState::new();

        // This is the gap between the UI flipping to CONNECTING and the
        // tunnel_start command actually subscribing.
        cancel.cancel();

        let (_, pending) = cancel.subscribe();
        assert!(pending, "the connect must see the early cancel");
    }
}
