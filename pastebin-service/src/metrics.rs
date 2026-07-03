//! Process-wide, **lock-free** service counters.
//!
//! These counters are shared across every handler through the single
//! `Arc<AppState>`. Each is an atomic, so a bump needs no lock and no `&mut`:
//! `fetch_add` mutates through `&self`, and many threads can increment the same
//! counter in parallel without blocking each other.
//!
//! Ordering is `Relaxed` on purpose: a counter doesn't *guard* any other memory,
//! so we don't need the happens-before guarantees of `Acquire`/`Release`. We only
//! need each increment to be atomic and the running total eventually accurate.
//! This is the one spot where "lock-free" is unambiguously right: a single
//! integer, safe, no `unsafe`, no dependency. See `docs/CONCURRENCY.md`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Lock-free counters for the service.
#[derive(Debug, Default)]
pub struct Metrics {
    pastes_served: AtomicU64,
}

impl Metrics {
    /// Count one successfully served paste fetch. Lock-free and non-blocking.
    pub fn record_served(&self) {
        self.pastes_served.fetch_add(1, Ordering::Relaxed);
    }

    /// Total paste fetches served since startup.
    pub fn pastes_served(&self) -> u64 {
        self.pastes_served.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_accumulate() {
        let m = Metrics::default();
        assert_eq!(m.pastes_served(), 0);
        m.record_served();
        m.record_served();
        assert_eq!(m.pastes_served(), 2);
    }

    // Many threads hammer the same counter; a lock-free atomic loses none.
    #[test]
    fn concurrent_increments_are_not_lost() {
        use std::sync::Arc;

        let m = Arc::new(Metrics::default());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let m = Arc::clone(&m);
            handles.push(std::thread::spawn(move || {
                for _ in 0..1_000 {
                    m.record_served();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(m.pastes_served(), 8 * 1_000);
    }
}
