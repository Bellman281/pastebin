//! Process-wide, **lock-free** server counters.
//!
//! Shared across every connection task through the single `Arc<AppState>`. Each
//! counter is an atomic, so a bump needs no lock and no `&mut` — `fetch_add`
//! mutates through `&self`, and many connection tasks increment in parallel
//! without blocking each other. `Relaxed` ordering is correct: the counters
//! guard no other memory; we only need each bump to be atomic and the running
//! total eventually accurate.
//!
//! *Active* connections are not tracked here — that count is owned by the
//! connection-registry actor (see `registry.rs`), which is the single source of
//! truth for the live connection set.

use std::sync::atomic::{AtomicU64, Ordering};

/// Lock-free monotonic counters.
#[derive(Debug, Default)]
pub struct Metrics {
    connections_total: AtomicU64,
    requests_total: AtomicU64,
}

impl Metrics {
    /// Count one accepted connection.
    pub fn on_connect(&self) {
        self.connections_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Count one served request.
    pub fn on_request(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Total connections accepted since startup.
    pub fn connections_total(&self) -> u64 {
        self.connections_total.load(Ordering::Relaxed)
    }

    /// Total requests served since startup.
    pub fn requests_total(&self) -> u64 {
        self.requests_total.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate() {
        let m = Metrics::default();
        assert_eq!(m.connections_total(), 0);
        m.on_connect();
        m.on_connect();
        m.on_request();
        assert_eq!(m.connections_total(), 2);
        assert_eq!(m.requests_total(), 1);
    }

    #[test]
    fn concurrent_increments_are_not_lost() {
        use std::sync::Arc;
        let m = Arc::new(Metrics::default());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let m = Arc::clone(&m);
            handles.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    m.on_request();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(m.requests_total(), 8000);
    }
}
