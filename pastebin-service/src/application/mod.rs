//! Application layer: use cases orchestrating the domain over a repository port.
//!
//! `PasteService` depends on `Arc<dyn PasteRepository>`, so it is unit-tested
//! against an in-memory double and runs in production against SQLite unchanged.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::cache::{Cache, NoOpCache};
use crate::domain::{
    BoxedError, Content, Paste, PasteId, PasteRepository, RepoError, ValidationError,
};
use crate::views::{ImmediateViewRecorder, ViewRecorder};

/// Length of an auto-generated paste id.
const GENERATED_ID_LEN: usize = 8;
/// Retry attempts on a (rare) id collision before giving up.
const MAX_GENERATION_ATTEMPTS: usize = 5;
/// Upper bound on how long a paste is cached (seconds).
const MAX_CACHE_TTL_SECS: u64 = 300;
/// Base62 alphabet for generated ids.
const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Errors surfaced by the application layer.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Validation(ValidationError),
    #[error("not found")]
    NotFound,
    #[error("paste id already in use")]
    Conflict,
    #[error(transparent)]
    Backend(BoxedError),
}

impl From<RepoError> for ServiceError {
    fn from(err: RepoError) -> Self {
        match err {
            RepoError::Conflict => ServiceError::Conflict,
            RepoError::Backend(cause) => ServiceError::Backend(cause),
        }
    }
}

/// Use cases for creating and fetching pastes.
#[derive(Clone)]
pub struct PasteService {
    repo: Arc<dyn PasteRepository>,
    /// Read-cache for the hot fetch path (NoOp when disabled).
    cache: Arc<dyn Cache>,
    /// Where paste views are recorded. Immediate by default; the composition
    /// root swaps in a channel-backed batcher for production.
    views: Arc<dyn ViewRecorder>,
}

impl PasteService {
    /// Construct with caching disabled (NoOp cache).
    pub fn new(repo: Arc<dyn PasteRepository>) -> Self {
        Self::with_cache(repo, Arc::new(NoOpCache))
    }

    /// Construct with an explicit cache and the default (immediate) view
    /// recorder — one DB write per view. Keeps counts exact, convenient for
    /// tests and single-instance runs.
    pub fn with_cache(repo: Arc<dyn PasteRepository>, cache: Arc<dyn Cache>) -> Self {
        let views: Arc<dyn ViewRecorder> = Arc::new(ImmediateViewRecorder::new(repo.clone()));
        Self { repo, cache, views }
    }

    /// Construct with an explicit cache *and* view recorder. Production passes a
    /// [`crate::views::BatchingViewRecorder`] so fetches only enqueue a view
    /// (non-blocking) and a background task batches the DB writes.
    pub fn with_cache_and_views(
        repo: Arc<dyn PasteRepository>,
        cache: Arc<dyn Cache>,
        views: Arc<dyn ViewRecorder>,
    ) -> Self {
        Self { repo, cache, views }
    }

    /// Create a paste from `content`, with optional syntax hint, TTL, and
    /// burn-after-read. The id is generated; a clash retries with a new id.
    pub async fn create(
        &self,
        content: String,
        syntax: Option<String>,
        ttl_seconds: Option<u64>,
        one_shot: bool,
    ) -> Result<Paste, ServiceError> {
        let content = Content::parse(content).map_err(ServiceError::Validation)?;
        let created_at = now_unix();
        let expires_at = ttl_seconds.map(|secs| created_at.saturating_add(secs as i64));

        for _ in 0..MAX_GENERATION_ATTEMPTS {
            let id = PasteId::from_trusted(generate_id(GENERATED_ID_LEN));
            let paste = Paste::new(
                id,
                content.clone(),
                syntax.clone(),
                created_at,
                expires_at,
                one_shot,
            );
            match self.repo.insert(&paste).await {
                Ok(()) => return Ok(paste),
                Err(RepoError::Conflict) => continue,
                Err(RepoError::Backend(cause)) => return Err(ServiceError::Backend(cause)),
            }
        }
        Err(ServiceError::Conflict)
    }

    /// Fetch a paste. Expired pastes are purged and reported as `NotFound`;
    /// a `one_shot` paste is deleted after this read (burn-after-read);
    /// otherwise the view counter is incremented. An invalid id cannot exist,
    /// so it maps to `NotFound`.
    pub async fn fetch(&self, id: String) -> Result<Paste, ServiceError> {
        let id = PasteId::parse(id).map_err(|_| ServiceError::NotFound)?;

        // Cache-aside. Only non-one-shot pastes are ever cached, so a cache hit
        // is always safe to serve (burn-after-read pastes always hit the DB so
        // they can be deleted).
        if let Some(json) = self.cache.get(id.as_str()).await {
            if let Ok(cached) = serde_json::from_str::<CachedPaste>(&json) {
                let paste = cached.into_paste();
                if paste.is_expired(now_unix()) {
                    self.cache.delete(id.as_str()).await;
                } else {
                    self.views.record(id).await; // non-blocking enqueue (or immediate)
                    return Ok(paste);
                }
            }
        }

        let paste = self.repo.get(&id).await?.ok_or(ServiceError::NotFound)?;

        if paste.is_expired(now_unix()) {
            let _ = self.repo.delete(&id).await; // best-effort lazy purge
            self.cache.delete(id.as_str()).await;
            return Err(ServiceError::NotFound);
        }

        if paste.one_shot {
            let _ = self.repo.delete(&id).await; // burn-after-read (never cached)
        } else {
            let ttl = cache_ttl_secs(&paste);
            if ttl > 0 {
                if let Ok(json) = serde_json::to_string(&CachedPaste::from_paste(&paste)) {
                    self.cache.set(id.as_str(), &json, ttl).await;
                }
            }
            // Counting a view must never fail (or block) a fetch, so it is
            // best-effort and fire-and-forget — not propagated with `?`.
            self.views.record(id).await;
        }
        Ok(paste)
    }

    /// Delete a paste, or `NotFound` if it does not exist.
    pub async fn delete(&self, id: String) -> Result<(), ServiceError> {
        let id = PasteId::parse(id).map_err(|_| ServiceError::NotFound)?;
        let removed = self.repo.delete(&id).await?;
        self.cache.delete(id.as_str()).await; // invalidate regardless
        if removed {
            Ok(())
        } else {
            Err(ServiceError::NotFound)
        }
    }

    /// Readiness check: confirm the backing store is reachable.
    pub async fn ready(&self) -> Result<(), ServiceError> {
        self.repo.ping().await?;
        Ok(())
    }
}

/// Current time as Unix seconds; clamps to 0 if the clock predates the epoch.
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Serialized snapshot of a paste for the cache, so the domain types stay
/// serde-free. Only non-one-shot pastes are ever cached.
#[derive(Serialize, Deserialize)]
struct CachedPaste {
    id: String,
    content: String,
    syntax: Option<String>,
    created_at: i64,
    expires_at: Option<i64>,
    one_shot: bool,
    views: i64,
}

impl CachedPaste {
    fn from_paste(p: &Paste) -> Self {
        Self {
            id: p.id.as_str().to_owned(),
            content: p.content.as_str().to_owned(),
            syntax: p.syntax.clone(),
            created_at: p.created_at,
            expires_at: p.expires_at,
            one_shot: p.one_shot,
            views: p.views,
        }
    }

    fn into_paste(self) -> Paste {
        Paste {
            id: PasteId::from_trusted(self.id),
            content: Content::from_trusted(self.content),
            syntax: self.syntax,
            created_at: self.created_at,
            expires_at: self.expires_at,
            one_shot: self.one_shot,
            views: self.views,
        }
    }
}

/// How long to cache a paste: bounded by `MAX_CACHE_TTL_SECS` and never beyond
/// the paste's own expiry. Returns 0 if already expired (don't cache).
fn cache_ttl_secs(paste: &Paste) -> u64 {
    match paste.expires_at {
        Some(exp) => {
            let remaining = exp - now_unix();
            if remaining <= 0 {
                0
            } else {
                (remaining as u64).min(MAX_CACHE_TTL_SECS)
            }
        }
        None => MAX_CACHE_TTL_SECS,
    }
}

/// Generate a random base62 id of the given length.
fn generate_id(len: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::InMemoryCache;
    use crate::domain::MAX_CONTENT_BYTES;
    use crate::infrastructure::InMemoryPasteRepository;

    fn service() -> PasteService {
        PasteService::new(Arc::new(InMemoryPasteRepository::default()))
    }

    #[tokio::test]
    async fn fetch_caches_non_one_shot_and_delete_invalidates() {
        let repo = Arc::new(InMemoryPasteRepository::default());
        let cache = Arc::new(InMemoryCache::default());
        let svc = PasteService::with_cache(repo.clone(), cache.clone());

        let p = svc
            .create("hello".to_owned(), None, None, false)
            .await
            .unwrap();
        // First fetch populates the cache.
        svc.fetch(p.id.as_str().to_owned()).await.unwrap();
        assert!(cache.get(p.id.as_str()).await.is_some());

        // Delete invalidates it.
        svc.delete(p.id.as_str().to_owned()).await.unwrap();
        assert!(cache.get(p.id.as_str()).await.is_none());
    }

    #[tokio::test]
    async fn one_shot_paste_is_never_cached() {
        let repo = Arc::new(InMemoryPasteRepository::default());
        let cache = Arc::new(InMemoryCache::default());
        let svc = PasteService::with_cache(repo.clone(), cache.clone());

        let p = svc
            .create("secret".to_owned(), None, None, true)
            .await
            .unwrap();
        // Fetch burns it; it must not be cached.
        svc.fetch(p.id.as_str().to_owned()).await.unwrap();
        assert!(cache.get(p.id.as_str()).await.is_none());
        // And it's gone (burned).
        assert!(matches!(
            svc.fetch(p.id.as_str().to_owned()).await,
            Err(ServiceError::NotFound)
        ));
    }

    #[test]
    fn generated_id_is_valid() {
        let id = generate_id(GENERATED_ID_LEN);
        assert_eq!(id.len(), GENERATED_ID_LEN);
        assert!(PasteId::parse(&id).is_ok());
    }

    #[tokio::test]
    async fn create_then_fetch_roundtrips() {
        let svc = service();
        let p = svc
            .create("hello world".to_owned(), Some("text".to_owned()), None, false)
            .await
            .unwrap();
        let fetched = svc.fetch(p.id.as_str().to_owned()).await.unwrap();
        assert_eq!(fetched.content.as_str(), "hello world");
        assert_eq!(fetched.syntax.as_deref(), Some("text"));
    }

    #[tokio::test]
    async fn create_rejects_empty_and_oversized_content() {
        let svc = service();
        assert!(matches!(
            svc.create(String::new(), None, None, false).await,
            Err(ServiceError::Validation(_))
        ));
        let big = "x".repeat(MAX_CONTENT_BYTES + 1);
        assert!(matches!(
            svc.create(big, None, None, false).await,
            Err(ServiceError::Validation(_))
        ));
    }

    #[tokio::test]
    async fn fetch_expired_is_not_found_and_purged() {
        let repo = Arc::new(InMemoryPasteRepository::default());
        let svc = PasteService::new(repo.clone());
        let expired = Paste::new(
            PasteId::parse("old").unwrap(),
            Content::parse("x").unwrap(),
            None,
            1_000,
            Some(1_001),
            false,
        );
        repo.insert(&expired).await.unwrap();

        assert!(matches!(
            svc.fetch("old".to_owned()).await,
            Err(ServiceError::NotFound)
        ));
        assert!(repo
            .get(&PasteId::parse("old").unwrap())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn one_shot_paste_is_burned_after_first_fetch() {
        let svc = service();
        let p = svc
            .create("secret".to_owned(), None, None, true)
            .await
            .unwrap();
        let first = svc.fetch(p.id.as_str().to_owned()).await.unwrap();
        assert_eq!(first.content.as_str(), "secret");
        assert!(matches!(
            svc.fetch(p.id.as_str().to_owned()).await,
            Err(ServiceError::NotFound)
        ));
    }

    #[tokio::test]
    async fn non_one_shot_fetch_increments_views() {
        let repo = Arc::new(InMemoryPasteRepository::default());
        let svc = PasteService::new(repo.clone());
        let p = svc
            .create("body".to_owned(), None, None, false)
            .await
            .unwrap();
        svc.fetch(p.id.as_str().to_owned()).await.unwrap();
        let stored = repo.get(&p.id).await.unwrap().unwrap();
        assert_eq!(stored.views, 1);
    }

    #[tokio::test]
    async fn missing_id_is_not_found() {
        let svc = service();
        assert!(matches!(
            svc.fetch("missing".to_owned()).await,
            Err(ServiceError::NotFound)
        ));
        assert!(matches!(
            svc.delete("missing".to_owned()).await,
            Err(ServiceError::NotFound)
        ));
    }

    #[tokio::test]
    async fn delete_removes_paste() {
        let svc = service();
        let p = svc
            .create("x".to_owned(), None, None, false)
            .await
            .unwrap();
        svc.delete(p.id.as_str().to_owned()).await.unwrap();
        assert!(matches!(
            svc.fetch(p.id.as_str().to_owned()).await,
            Err(ServiceError::NotFound)
        ));
    }
}
