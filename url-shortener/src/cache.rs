//! A small read-cache abstraction for the hot redirect path.
//!
//! The `Cache` port stores `code -> target` (immutable for a link's life), so a
//! cache hit serves a redirect without touching the database. All operations
//! are **best-effort**: a cache error degrades to a miss / no-op, never an error
//! to the client (the DB remains the source of truth).
//!
//! Implementations:
//! - [`NoOpCache`] — disabled (default; the app runs fine without Redis).
//! - [`InMemoryCache`] — process-local map; used to unit-test caching logic.
//! - [`RedisCache`] — production, shared across instances.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::domain::BoxedError;

/// Read-cache port. Best-effort: errors are swallowed (logged in real impls).
#[async_trait::async_trait]
pub trait Cache: Send + Sync + 'static {
    async fn get(&self, key: &str) -> Option<String>;
    async fn set(&self, key: &str, value: &str, ttl_secs: u64);
    async fn delete(&self, key: &str);
}

/// Caching disabled — every read misses; writes are no-ops.
#[derive(Debug, Default)]
pub struct NoOpCache;

#[async_trait::async_trait]
impl Cache for NoOpCache {
    async fn get(&self, _key: &str) -> Option<String> {
        None
    }
    async fn set(&self, _key: &str, _value: &str, _ttl_secs: u64) {}
    async fn delete(&self, _key: &str) {}
}

/// Process-local cache (ignores TTL). For tests and single-instance local runs.
#[derive(Debug, Default)]
pub struct InMemoryCache {
    entries: Mutex<HashMap<String, String>>,
}

impl InMemoryCache {
    fn guard(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait::async_trait]
impl Cache for InMemoryCache {
    async fn get(&self, key: &str) -> Option<String> {
        self.guard().get(key).cloned()
    }
    async fn set(&self, key: &str, value: &str, _ttl_secs: u64) {
        self.guard().insert(key.to_owned(), value.to_owned());
    }
    async fn delete(&self, key: &str) {
        self.guard().remove(key);
    }
}

/// Redis-backed cache shared across instances. All ops are best-effort: any
/// Redis error is logged and treated as a miss / no-op, so the service falls
/// back to the database.
pub struct RedisCache {
    manager: redis::aio::ConnectionManager,
}

impl RedisCache {
    /// Connect and build a multiplexed, auto-reconnecting connection manager.
    pub async fn connect(url: &str) -> Result<Self, BoxedError> {
        let client = redis::Client::open(url)?;
        let manager = redis::aio::ConnectionManager::new(client).await?;
        Ok(Self { manager })
    }
}

#[async_trait::async_trait]
impl Cache for RedisCache {
    async fn get(&self, key: &str) -> Option<String> {
        use redis::AsyncCommands;
        let mut conn = self.manager.clone();
        match conn.get::<_, Option<String>>(key).await {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(error = %err, "redis get failed; treating as miss");
                None
            }
        }
    }

    async fn set(&self, key: &str, value: &str, ttl_secs: u64) {
        use redis::AsyncCommands;
        let mut conn = self.manager.clone();
        let result: redis::RedisResult<()> = if ttl_secs > 0 {
            conn.set_ex(key, value, ttl_secs).await
        } else {
            conn.set(key, value).await
        };
        if let Err(err) = result {
            tracing::warn!(error = %err, "redis set failed");
        }
    }

    async fn delete(&self, key: &str) {
        use redis::AsyncCommands;
        let mut conn = self.manager.clone();
        let result: redis::RedisResult<()> = conn.del(key).await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "redis del failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_cache_roundtrips_and_deletes() {
        let cache = InMemoryCache::default();
        assert_eq!(cache.get("k").await, None);
        cache.set("k", "v", 60).await;
        assert_eq!(cache.get("k").await.as_deref(), Some("v"));
        cache.delete("k").await;
        assert_eq!(cache.get("k").await, None);
    }

    #[tokio::test]
    async fn noop_cache_never_stores() {
        let cache = NoOpCache;
        cache.set("k", "v", 60).await;
        assert_eq!(cache.get("k").await, None);
    }
}
