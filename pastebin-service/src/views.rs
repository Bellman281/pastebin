//! View counting via a background batcher — the channel / actor pattern.
//!
//! Fetching a paste shouldn't wait on a database write just to bump a counter.
//! A non-one-shot fetch **enqueues** the paste id on an in-process channel (a
//! non-blocking send) and returns immediately; a single background task owns the
//! aggregation, coalescing many views for the same id into one batched
//! `views = views + n` write that it flushes on an interval.
//!
//! This is the right place for channels here: view counting is *fire-and-forget*
//! (already best-effort) and *batchable*. Burn-after-read (`one_shot`) pastes are
//! NOT counted this way — they are deleted synchronously on first read, which
//! must not be deferred. See `docs/CONCURRENCY.md`.
//!
//! Two implementations sit behind the [`ViewRecorder`] port:
//! - [`ImmediateViewRecorder`] — one write per view. Exact and synchronous; the
//!   default, and convenient for deterministic tests.
//! - [`BatchingViewRecorder`] — the channel batcher above, wired in `main`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::domain::{PasteId, PasteRepository};

/// Default cap on how stale a buffered count can get before it is flushed.
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(2);
/// Default number of *distinct* ids buffered before an early flush is forced.
pub const DEFAULT_MAX_PENDING: usize = 1024;

/// Records a paste view. Best-effort and non-blocking: a failed or dropped count
/// must never fail (or slow) a fetch.
#[async_trait::async_trait]
pub trait ViewRecorder: Send + Sync + 'static {
    /// Record one view. Implementations must not block the caller on I/O.
    async fn record(&self, id: PasteId);

    /// Flush any buffered counts to the store. Default: nothing is buffered, so
    /// this is a no-op. The batcher overrides it for graceful shutdown.
    async fn flush(&self) {}
}

/// Increments immediately — one DB write per view. Exact and simple; used by
/// default (see `PasteService::with_cache`) and in unit tests where immediate,
/// deterministic counts are convenient.
pub struct ImmediateViewRecorder {
    repo: Arc<dyn PasteRepository>,
}

impl ImmediateViewRecorder {
    pub fn new(repo: Arc<dyn PasteRepository>) -> Self {
        Self { repo }
    }
}

#[async_trait::async_trait]
impl ViewRecorder for ImmediateViewRecorder {
    async fn record(&self, id: PasteId) {
        // Best-effort: a failed counter write must never fail a fetch.
        let _ = self.repo.increment_views(&id).await;
    }
}

/// Messages the background task understands.
enum Msg {
    /// One observed view for a paste id.
    View(PasteId),
    /// Flush now and acknowledge on the oneshot (used by `flush`/shutdown).
    Flush(oneshot::Sender<()>),
}

/// Batches views in a single background task and flushes them periodically. The
/// hot path only does a non-blocking channel send; all map mutation and DB
/// writes happen on the owning task, so there is no shared lock at all.
pub struct BatchingViewRecorder {
    tx: mpsc::UnboundedSender<Msg>,
}

impl BatchingViewRecorder {
    /// Spawn the background batcher.
    ///
    /// - `interval` bounds how stale a count can be (flushed at least this often).
    /// - `max_pending` forces an early flush once that many *distinct* ids are
    ///   buffered, bounding memory under a burst.
    ///
    /// Returns the recorder plus the task's [`JoinHandle`] so the composition
    /// root can await/abort it on shutdown.
    pub fn spawn(
        repo: Arc<dyn PasteRepository>,
        interval: Duration,
        max_pending: usize,
    ) -> (Arc<Self>, JoinHandle<()>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(run(repo, rx, interval, max_pending.max(1)));
        (Arc::new(Self { tx }), handle)
    }

    /// Spawn with the default interval and pending cap.
    pub fn spawn_default(repo: Arc<dyn PasteRepository>) -> (Arc<Self>, JoinHandle<()>) {
        Self::spawn(repo, DEFAULT_FLUSH_INTERVAL, DEFAULT_MAX_PENDING)
    }
}

#[async_trait::async_trait]
impl ViewRecorder for BatchingViewRecorder {
    async fn record(&self, id: PasteId) {
        // If the receiver is gone (shutting down), silently drop — best-effort.
        let _ = self.tx.send(Msg::View(id));
    }

    async fn flush(&self) {
        let (ack, done) = oneshot::channel();
        // If the task is already gone, there is nothing left to flush.
        if self.tx.send(Msg::Flush(ack)).is_ok() {
            let _ = done.await;
        }
    }
}

/// The actor loop: own the pending map, drain the channel, flush on a timer or
/// on demand, and do a final flush when all senders drop (no lost counts).
async fn run(
    repo: Arc<dyn PasteRepository>,
    mut rx: mpsc::UnboundedReceiver<Msg>,
    interval: Duration,
    max_pending: usize,
) {
    let mut pending: HashMap<String, i64> = HashMap::new();
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            maybe = rx.recv() => match maybe {
                Some(Msg::View(id)) => {
                    *pending.entry(id.as_str().to_owned()).or_insert(0) += 1;
                    if pending.len() >= max_pending {
                        flush(&repo, &mut pending).await;
                    }
                }
                Some(Msg::Flush(ack)) => {
                    flush(&repo, &mut pending).await;
                    let _ = ack.send(());
                }
                None => {
                    // All senders dropped: final flush, then exit.
                    flush(&repo, &mut pending).await;
                    return;
                }
            },
            _ = ticker.tick() => {
                flush(&repo, &mut pending).await;
            }
        }
    }
}

/// Write every buffered count as a single `+= n` per id, then clear. Draining
/// (rather than retaining) keeps a failed write from being retried forever —
/// consistent with best-effort view counting.
async fn flush(repo: &Arc<dyn PasteRepository>, pending: &mut HashMap<String, i64>) {
    if pending.is_empty() {
        return;
    }
    for (id, n) in pending.drain() {
        let id = PasteId::from_trusted(id);
        let _ = repo.increment_views_by(&id, n).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Content, Paste};
    use crate::infrastructure::InMemoryPasteRepository;

    async fn repo_with(id: &str) -> Arc<InMemoryPasteRepository> {
        let repo = Arc::new(InMemoryPasteRepository::default());
        let paste = Paste::new(
            PasteId::parse(id).unwrap(),
            Content::parse("body").unwrap(),
            None,
            1_700_000_000,
            None,
            false,
        );
        repo.insert(&paste).await.unwrap();
        repo
    }

    async fn views_of(repo: &Arc<InMemoryPasteRepository>, id: &str) -> i64 {
        repo.get(&PasteId::parse(id).unwrap())
            .await
            .unwrap()
            .unwrap()
            .views
    }

    #[tokio::test]
    async fn immediate_recorder_counts_each_view() {
        let repo = repo_with("abc").await;
        let rec = ImmediateViewRecorder::new(repo.clone());
        rec.record(PasteId::parse("abc").unwrap()).await;
        rec.record(PasteId::parse("abc").unwrap()).await;
        assert_eq!(views_of(&repo, "abc").await, 2);
    }

    #[tokio::test]
    async fn batcher_coalesces_views_and_flush_persists_them() {
        let repo = repo_with("abc").await;
        let (rec, _h) =
            BatchingViewRecorder::spawn(repo.clone(), Duration::from_secs(3600), 1_000_000);
        for _ in 0..5 {
            rec.record(PasteId::parse("abc").unwrap()).await;
        }
        rec.flush().await;
        assert_eq!(views_of(&repo, "abc").await, 5);
    }

    #[tokio::test]
    async fn dropping_the_recorder_flushes_remaining_counts() {
        let repo = repo_with("xy").await;
        let (rec, handle) =
            BatchingViewRecorder::spawn(repo.clone(), Duration::from_secs(3600), 1_000_000);
        rec.record(PasteId::parse("xy").unwrap()).await;
        rec.record(PasteId::parse("xy").unwrap()).await;
        drop(rec); // last sender gone -> task does a final flush and exits
        handle.await.unwrap();
        assert_eq!(views_of(&repo, "xy").await, 2);
    }
}
