//! In-memory [`LinkRepository`] implemented as an **actor**: a single
//! background task owns the `HashMap` outright and is the only code that ever
//! touches it. Callers never share the map — they send a command down an
//! `mpsc` channel and await that one call's answer on a `oneshot` reply channel.
//!
//! This is *share memory by communicating*: there is **no `Mutex`, no lock
//! guard, and no lock poisoning**. The map is plain owned state, mutated through
//! `&mut` on one task, so data races are impossible by construction rather than
//! by discipline. The trade-off — and the reason production uses SQLite, not
//! this — is that every operation, including reads, is a channel round-trip
//! serialized onto the single owning task.
//!
//! Lifecycle: the task lives as long as any sender clone exists. When the last
//! handle is dropped the channel closes, the task's `recv()` returns `None`, and
//! the task exits — the map is freed, nothing leaks.

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};

use crate::domain::{BoxedError, Link, LinkRepository, RepoError, ShortCode};

/// Commands the owning task understands. Each carries a `oneshot` sender on
/// which the task returns the result of that single call.
enum Cmd {
    /// Insert a link; reply `true` if inserted, `false` if the code was taken.
    Insert {
        link: Link,
        reply: oneshot::Sender<bool>,
    },
    /// Fetch a link by code.
    Get {
        code: String,
        reply: oneshot::Sender<Option<Link>>,
    },
    /// Add `n` to a link's hit count; reply `true` if the row existed.
    IncrementBy {
        code: String,
        n: i64,
        reply: oneshot::Sender<bool>,
    },
    /// Remove a link; reply `true` if a row was removed.
    Delete {
        code: String,
        reply: oneshot::Sender<bool>,
    },
    /// Liveness round-trip (proves the owning task is still running).
    Ping { reply: oneshot::Sender<()> },
}

/// In-memory link store backed by an owning task — no shared lock.
///
/// Cloning yields another handle to the *same* task (and therefore the same
/// data), so wrapping it in `Arc` and cloning the `Arc` both share one store.
#[derive(Debug, Clone)]
pub struct InMemoryLinkRepository {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl Default for InMemoryLinkRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryLinkRepository {
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
    let mut links: HashMap<String, Link> = HashMap::new();
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Insert { link, reply } => {
                use std::collections::hash_map::Entry;
                let inserted = match links.entry(link.code.as_str().to_owned()) {
                    Entry::Occupied(_) => false, // code already taken -> Conflict
                    Entry::Vacant(slot) => {
                        slot.insert(link);
                        true
                    }
                };
                let _ = reply.send(inserted);
            }
            Cmd::Get { code, reply } => {
                let _ = reply.send(links.get(&code).cloned());
            }
            Cmd::IncrementBy { code, n, reply } => {
                let updated = match links.get_mut(&code) {
                    Some(link) => {
                        link.hits += n;
                        true
                    }
                    None => false,
                };
                let _ = reply.send(updated);
            }
            Cmd::Delete { code, reply } => {
                let _ = reply.send(links.remove(&code).is_some());
            }
            Cmd::Ping { reply } => {
                let _ = reply.send(());
            }
        }
    }
    // Channel closed: all handles dropped. The map drops here — nothing leaks.
}

#[async_trait::async_trait]
impl LinkRepository for InMemoryLinkRepository {
    async fn insert(&self, link: &Link) -> Result<(), RepoError> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(Cmd::Insert {
            link: link.clone(),
            reply,
        })?;
        match rx.await {
            Ok(true) => Ok(()),
            Ok(false) => Err(RepoError::Conflict),
            Err(_) => Err(actor_stopped()),
        }
    }

    async fn get(&self, code: &ShortCode) -> Result<Option<Link>, RepoError> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(Cmd::Get {
            code: code.as_str().to_owned(),
            reply,
        })?;
        rx.await.map_err(|_| actor_stopped())
    }

    async fn increment_hits_by(&self, code: &ShortCode, n: i64) -> Result<bool, RepoError> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(Cmd::IncrementBy {
            code: code.as_str().to_owned(),
            n,
            reply,
        })?;
        rx.await.map_err(|_| actor_stopped())
    }

    async fn delete(&self, code: &ShortCode) -> Result<bool, RepoError> {
        let (reply, rx) = oneshot::channel();
        self.dispatch(Cmd::Delete {
            code: code.as_str().to_owned(),
            reply,
        })?;
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
    use crate::domain::{ShortCode, TargetUrl};

    fn sample() -> Link {
        Link::new(
            ShortCode::parse("abc").unwrap(),
            TargetUrl::parse("https://example.com").unwrap(),
            1_700_000_000,
        )
    }

    #[tokio::test]
    async fn insert_is_idempotent_guarded_by_conflict() {
        let repo = InMemoryLinkRepository::default();
        repo.insert(&sample()).await.unwrap();
        assert!(matches!(
            repo.insert(&sample()).await,
            Err(RepoError::Conflict)
        ));
    }

    #[tokio::test]
    async fn increment_and_delete_report_presence() {
        let repo = InMemoryLinkRepository::default();
        let code = ShortCode::parse("abc").unwrap();
        assert!(!repo.increment_hits(&code).await.unwrap());
        repo.insert(&sample()).await.unwrap();
        assert!(repo.increment_hits(&code).await.unwrap());
        assert_eq!(repo.get(&code).await.unwrap().unwrap().hits, 1);
        assert!(repo.delete(&code).await.unwrap());
        assert!(!repo.delete(&code).await.unwrap());
    }

    #[tokio::test]
    async fn increment_hits_by_adds_a_batch_in_one_call() {
        let repo = InMemoryLinkRepository::default();
        let code = ShortCode::parse("abc").unwrap();
        assert!(!repo.increment_hits_by(&code, 5).await.unwrap()); // absent
        repo.insert(&sample()).await.unwrap();
        assert!(repo.increment_hits_by(&code, 5).await.unwrap());
        repo.increment_hits(&code).await.unwrap(); // default delegates to +1
        assert_eq!(repo.get(&code).await.unwrap().unwrap().hits, 6);
    }

    #[tokio::test]
    async fn ping_round_trips_through_the_task() {
        let repo = InMemoryLinkRepository::default();
        assert!(repo.ping().await.is_ok());
    }

    // Many tasks share one store (via cloned handles) and the owning task
    // serializes their writes, so every distinct insert lands exactly once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_inserts_are_serialized_by_the_owning_task() {
        use std::sync::Arc;

        let repo = Arc::new(InMemoryLinkRepository::default());
        let mut handles = Vec::new();
        for i in 0..50 {
            let repo = repo.clone();
            handles.push(tokio::spawn(async move {
                let link = Link::new(
                    ShortCode::parse(format!("code{i:03}")).unwrap(),
                    TargetUrl::parse("https://example.com").unwrap(),
                    1_700_000_000,
                );
                repo.insert(&link).await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        for i in 0..50 {
            let code = ShortCode::parse(format!("code{i:03}")).unwrap();
            assert!(repo.get(&code).await.unwrap().is_some());
        }
    }
}
