//! In-memory [`PasteRepository`] implemented as an **actor**: a single
//! background task owns the `HashMap` outright and is the only code that ever
//! touches it. Callers never share the map — they send a command down an
//! `mpsc` channel and await that one call's answer on a `oneshot` reply channel.
//!
//! This is *share memory by communicating*: there is **no `Mutex`, no lock
//! guard, and no lock poisoning**. The map is plain owned state, mutated through
//! `&mut` on one task, so data races are impossible by construction. The
//! trade-off — and the reason production uses SQLite — is that every operation,
//! including reads, is a channel round-trip serialized on the owning task.
//! (See `docs/CONCURRENCY.md` and the url-shortener's matching design.)
//!
//! Lifecycle: the task lives as long as any sender clone exists; when the last
//! handle drops, the channel closes, `recv()` returns `None`, and the task
//! exits — the map is freed, nothing leaks.

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};

use crate::domain::{BoxedError, Paste, PasteId, PasteRepository, RepoError};

/// Commands the owning task understands. Each carries a `oneshot` sender on
/// which the task returns the result of that single call.
enum Cmd {
    /// Insert a paste; reply `true` if inserted, `false` if the id was taken.
    Insert { paste: Paste, reply: oneshot::Sender<bool> },
    /// Fetch a paste by id.
    Get { id: String, reply: oneshot::Sender<Option<Paste>> },
    /// Add `n` to a paste's view count; reply `true` if the row existed.
    IncrementBy { id: String, n: i64, reply: oneshot::Sender<bool> },
    /// Remove a paste; reply `true` if a row was removed.
    Delete { id: String, reply: oneshot::Sender<bool> },
    /// Liveness round-trip (proves the owning task is still running).
    Ping { reply: oneshot::Sender<()> },
}

/// In-memory paste store backed by an owning task — no shared lock.
///
/// Cloning yields another handle to the *same* task (and therefore the same
/// data), so wrapping it in `Arc` and cloning the `Arc` both share one store.
#[derive(Debug, Clone)]
pub struct InMemoryPasteRepository {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl Default for InMemoryPasteRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryPasteRepository {
    /// Spawn the owning task and return a handle that talks to it.
    ///
    /// Must be called from within a Tokio runtime (always true in this app — at
    /// runtime and in every `#[tokio::test]`).
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(rx));
        Self { tx }
    }

    /// Hand a command to the owning task, mapping a stopped task to a backend
    /// error (the send fails only if the task — and thus the receiver — is gone).
    fn dispatch(&self, cmd: Cmd) -> Result<(), RepoError> {
        self.tx.send(cmd).map_err(|_| actor_stopped())
    }
}

/// The error returned when the owning task is no longer running.
fn actor_stopped() -> RepoError {
    let cause: BoxedError = "in-memory repository task has stopped".into();
    RepoError::Backend(cause)
}

/// The actor loop: sole owner of the map, processing one command at a time.
async fn run(mut rx: mpsc::UnboundedReceiver<Cmd>) {
    let mut pastes: HashMap<String, Paste> = HashMap::new();
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Insert { paste, reply } => {
                use std::collections::hash_map::Entry;
                let inserted = match pastes.entry(paste.id.as_str().to_owned()) {
                    Entry::Occupied(_) => false, // id already taken -> Conflict
                    Entry::Vacant(slot) => {
                        slot.insert(paste);
                        true
                    }
                };
                let _ = reply.send(inserted);
            }
            Cmd::Get { id, reply } => {
                let _ = reply.send(pastes.get(&id).cloned());
            }
            Cmd::IncrementBy { id, n, reply } => {
                let updated = match pastes.get_mut(&id) {
                    Some(paste) => {
                        paste.views += n;
                        true
                    }
                    None => false,
                };
                let _ = reply.send(updated);
            }
            Cmd::Delete { id, reply } => {
                let _ = reply.send(pastes.remove(&id).is_some());
            }
            Cmd::Ping { reply } => {
                let _ = reply.send(());
            }
        }
    }
    // Channel closed: all handles dropped. The map drops here — nothing leaks.
}

#[async_trait::async_trait]
impl PasteRepository for InMemoryPasteRepository {
    async fn insert(&self, paste: &Paste) -> Result<(), RepoError> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(Cmd::Insert { paste: paste.clone(), reply })?;
        match rx.await {
            Ok(true) => Ok(()),
            Ok(false) => Err(RepoError::Conflict),
            Err(_) => Err(actor_stopped()),
        }
    }

    async fn get(&self, id: &PasteId) -> Result<Option<Paste>, RepoError> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(Cmd::Get { id: id.as_str().to_owned(), reply })?;
        rx.await.map_err(|_| actor_stopped())
    }

    async fn increment_views_by(&self, id: &PasteId, n: i64) -> Result<bool, RepoError> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(Cmd::IncrementBy { id: id.as_str().to_owned(), n, reply })?;
        rx.await.map_err(|_| actor_stopped())
    }

    async fn delete(&self, id: &PasteId) -> Result<bool, RepoError> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(Cmd::Delete { id: id.as_str().to_owned(), reply })?;
        rx.await.map_err(|_| actor_stopped())
    }

    async fn ping(&self) -> Result<(), RepoError> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(Cmd::Ping { reply })?;
        rx.await.map_err(|_| actor_stopped())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Content;

    fn sample() -> Paste {
        Paste::new(
            PasteId::parse("abc").unwrap(),
            Content::parse("body").unwrap(),
            None,
            1_700_000_000,
            None,
            false,
        )
    }

    #[tokio::test]
    async fn insert_conflicts_then_views_and_delete_report_presence() {
        let repo = InMemoryPasteRepository::default();
        let id = PasteId::parse("abc").unwrap();

        assert!(!repo.increment_views(&id).await.unwrap());
        repo.insert(&sample()).await.unwrap();
        assert!(matches!(repo.insert(&sample()).await, Err(RepoError::Conflict)));

        assert!(repo.increment_views(&id).await.unwrap());
        assert_eq!(repo.get(&id).await.unwrap().unwrap().views, 1);

        assert!(repo.delete(&id).await.unwrap());
        assert!(!repo.delete(&id).await.unwrap());
    }

    #[tokio::test]
    async fn increment_views_by_adds_a_batch_in_one_call() {
        let repo = InMemoryPasteRepository::default();
        let id = PasteId::parse("abc").unwrap();
        assert!(!repo.increment_views_by(&id, 5).await.unwrap()); // absent
        repo.insert(&sample()).await.unwrap();
        assert!(repo.increment_views_by(&id, 5).await.unwrap());
        repo.increment_views(&id).await.unwrap(); // default delegates to +1
        assert_eq!(repo.get(&id).await.unwrap().unwrap().views, 6);
    }

    #[tokio::test]
    async fn ping_round_trips_through_the_task() {
        let repo = InMemoryPasteRepository::default();
        assert!(repo.ping().await.is_ok());
    }

    // Many tasks share one store (via cloned handles) and the owning task
    // serializes their writes, so every distinct insert lands exactly once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_inserts_are_serialized_by_the_owning_task() {
        use std::sync::Arc;

        let repo = Arc::new(InMemoryPasteRepository::default());
        let mut handles = Vec::new();
        for i in 0..50 {
            let repo = repo.clone();
            handles.push(tokio::spawn(async move {
                let paste = Paste::new(
                    PasteId::parse(format!("id{i:04}")).unwrap(),
                    Content::parse("body").unwrap(),
                    None,
                    1_700_000_000,
                    None,
                    false,
                );
                repo.insert(&paste).await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        for i in 0..50 {
            let id = PasteId::parse(format!("id{i:04}")).unwrap();
            assert!(repo.get(&id).await.unwrap().is_some());
        }
    }
}
