//! In-memory [`LinkRepository`] backed by a `Mutex<HashMap>`.
//!
//! Memory notes: a single `HashMap` owns every link; entries are removed on
//! delete, so memory tracks live links exactly (no leak). The lock is held only
//! for the duration of a synchronous map operation — never across an `.await` —
//! so the futures stay `Send` and the lock never blocks the async runtime.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::domain::{Link, LinkRepository, RepoError, ShortCode};

/// Thread-safe in-memory link store.
#[derive(Debug, Default)]
pub struct InMemoryLinkRepository {
    links: Mutex<HashMap<String, Link>>,
}

impl InMemoryLinkRepository {
    /// Acquire the lock, recovering the data even if a previous holder panicked
    /// (poisoning), so a single panic cannot wedge the whole store.
    fn guard(&self) -> std::sync::MutexGuard<'_, HashMap<String, Link>> {
        self.links.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait::async_trait]
impl LinkRepository for InMemoryLinkRepository {
    async fn insert(&self, link: &Link) -> Result<(), RepoError> {
        let mut links = self.guard();
        if links.contains_key(link.code.as_str()) {
            return Err(RepoError::Conflict);
        }
        links.insert(link.code.as_str().to_owned(), link.clone());
        Ok(())
    }

    async fn get(&self, code: &ShortCode) -> Result<Option<Link>, RepoError> {
        Ok(self.guard().get(code.as_str()).cloned())
    }

    async fn increment_hits(&self, code: &ShortCode) -> Result<bool, RepoError> {
        match self.guard().get_mut(code.as_str()) {
            Some(link) => {
                link.hits += 1;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn delete(&self, code: &ShortCode) -> Result<bool, RepoError> {
        Ok(self.guard().remove(code.as_str()).is_some())
    }

    async fn ping(&self) -> Result<(), RepoError> {
        Ok(())
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
        assert!(matches!(repo.insert(&sample()).await, Err(RepoError::Conflict)));
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
}
