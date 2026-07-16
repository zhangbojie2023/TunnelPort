use std::sync::atomic::{AtomicU64, Ordering};

/// Thread-safe counters for proxy traffic (bytes transferred through the HTTP bridge).
pub struct ProxyStats {
    pub bytes_up: AtomicU64,   // client → server (upload)
    pub bytes_down: AtomicU64, // server → client (download)
}

impl Default for ProxyStats {
    fn default() -> Self {
        Self {
            bytes_up: AtomicU64::new(0),
            bytes_down: AtomicU64::new(0),
        }
    }
}

impl ProxyStats {
    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.bytes_up.load(Ordering::Relaxed),
            self.bytes_down.load(Ordering::Relaxed),
        )
    }

    pub fn reset(&self) {
        self.bytes_up.store(0, Ordering::Relaxed);
        self.bytes_down.store(0, Ordering::Relaxed);
    }
}
