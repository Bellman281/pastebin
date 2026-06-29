//! Application layer: use cases orchestrating the domain over a repository port.
//!
//! `LinkService` depends on `Arc<dyn LinkRepository>` — an abstraction — so it
//! is unit-tested against an in-memory double and runs in production against
//! SQLite without changing a line here.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::Rng;

use crate::domain::{
    BoxedError, Link, LinkRepository, RepoError, ShortCode, TargetUrl, ValidationError,
};

/// Length of an auto-generated short code.
const GENERATED_CODE_LEN: usize = 7;
/// How many times to retry generation on a (rare) collision before giving up.
const MAX_GENERATION_ATTEMPTS: usize = 5;
/// Base62 alphabet used for generated codes.
const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Errors surfaced by the application layer.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Validation(ValidationError),
    #[error("not found")]
    NotFound,
    #[error("short code already in use")]
    Conflict,
    #[error("target host is not allowed")]
    Blocked,
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

/// Use cases for creating and resolving short links.
#[derive(Clone)]
pub struct LinkService {
    repo: Arc<dyn LinkRepository>,
    /// Hosts (and their subdomains) that may not be shortened. Lowercased.
    blocked_hosts: Vec<String>,
}

impl LinkService {
    pub fn new(repo: Arc<dyn LinkRepository>, blocked_hosts: Vec<String>) -> Self {
        Self { repo, blocked_hosts }
    }

    /// True if `target`'s host is on the denylist (exact host or a subdomain).
    fn is_blocked(&self, target: &TargetUrl) -> bool {
        if self.blocked_hosts.is_empty() {
            return false;
        }
        match target.host() {
            Some(host) => self
                .blocked_hosts
                .iter()
                .any(|b| host == *b || host.ends_with(&format!(".{b}"))),
            None => false,
        }
    }

    /// Create a link for `url`. When `alias` is given it is used verbatim (and a
    /// clash is a hard conflict); otherwise a unique code is generated.
    pub async fn create(
        &self,
        url: String,
        alias: Option<String>,
        ttl_seconds: Option<u64>,
    ) -> Result<Link, ServiceError> {
        let target = TargetUrl::parse(url).map_err(ServiceError::Validation)?;
        if self.is_blocked(&target) {
            return Err(ServiceError::Blocked);
        }
        let created_at = now_unix();
        let expires_at = ttl_seconds.map(|secs| created_at.saturating_add(secs as i64));

        if let Some(alias) = alias {
            let code = ShortCode::parse(alias).map_err(ServiceError::Validation)?;
            let link = Link::with_expiry(code, target, created_at, expires_at);
            self.repo.insert(&link).await?; // RepoError -> ServiceError via From
            return Ok(link);
        }

        for _ in 0..MAX_GENERATION_ATTEMPTS {
            let code = ShortCode::from_trusted(generate_code(GENERATED_CODE_LEN));
            let link = Link::with_expiry(code, target.clone(), created_at, expires_at);
            match self.repo.insert(&link).await {
                Ok(()) => return Ok(link),
                Err(RepoError::Conflict) => continue,
                Err(RepoError::Backend(cause)) => return Err(ServiceError::Backend(cause)),
            }
        }
        // Exhausting retries is effectively impossible for a 7-char base62 space;
        // surfacing a conflict is the honest outcome if it ever happens.
        Err(ServiceError::Conflict)
    }

    /// Resolve a code to its target, counting the hit. An invalid code cannot
    /// exist, so it maps to `NotFound` rather than a validation error.
    pub async fn resolve(&self, code: String) -> Result<TargetUrl, ServiceError> {
        let code = ShortCode::parse(code).map_err(|_| ServiceError::NotFound)?;
        let link = self.repo.get(&code).await?.ok_or(ServiceError::NotFound)?;
        if link.is_expired(now_unix()) {
            let _ = self.repo.delete(&code).await; // best-effort lazy purge
            return Err(ServiceError::NotFound);
        }
        self.repo.increment_hits(&code).await?;
        Ok(link.target)
    }

    /// Fetch link metadata without counting a hit.
    pub async fn get(&self, code: String) -> Result<Link, ServiceError> {
        let code = ShortCode::parse(code).map_err(|_| ServiceError::NotFound)?;
        let link = self.repo.get(&code).await?.ok_or(ServiceError::NotFound)?;
        if link.is_expired(now_unix()) {
            let _ = self.repo.delete(&code).await;
            return Err(ServiceError::NotFound);
        }
        Ok(link)
    }

    /// Readiness check: confirm the backing store is reachable.
    pub async fn ready(&self) -> Result<(), ServiceError> {
        self.repo.ping().await?;
        Ok(())
    }

    /// Delete a link, or `NotFound` if it does not exist.
    pub async fn delete(&self, code: String) -> Result<(), ServiceError> {
        let code = ShortCode::parse(code).map_err(|_| ServiceError::NotFound)?;
        if self.repo.delete(&code).await? {
            Ok(())
        } else {
            Err(ServiceError::NotFound)
        }
    }
}

/// Current time as Unix seconds; clamps to 0 if the system clock predates the
/// epoch rather than panicking.
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Generate a random base62 code of the given length.
fn generate_code(len: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::InMemoryLinkRepository;

    fn service() -> LinkService {
        LinkService::new(Arc::new(InMemoryLinkRepository::default()), Vec::new())
    }

    #[test]
    fn generated_code_is_valid() {
        let code = generate_code(GENERATED_CODE_LEN);
        assert_eq!(code.len(), GENERATED_CODE_LEN);
        assert!(ShortCode::parse(&code).is_ok());
    }

    #[tokio::test]
    async fn create_then_get_roundtrips() {
        let svc = service();
        let link = svc
            .create("https://example.com".to_owned(), None, None)
            .await
            .unwrap();
        let fetched = svc.get(link.code.as_str().to_owned()).await.unwrap();
        assert_eq!(fetched.target.as_str(), "https://example.com");
        assert_eq!(fetched.hits, 0);
    }

    #[tokio::test]
    async fn create_rejects_invalid_url() {
        let svc = service();
        let err = svc
            .create("ftp://nope".to_owned(), None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::Validation(_)));
    }

    #[tokio::test]
    async fn custom_alias_conflict_is_reported() {
        let svc = service();
        svc.create("https://a.com".to_owned(), Some("mylink".to_owned()), None)
            .await
            .unwrap();
        let err = svc
            .create("https://b.com".to_owned(), Some("mylink".to_owned()), None)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::Conflict));
    }

    #[tokio::test]
    async fn resolve_increments_hits_and_returns_target() {
        let svc = service();
        let link = svc
            .create("https://example.com".to_owned(), Some("xy".to_owned()), None)
            .await
            .unwrap();
        let target = svc.resolve(link.code.as_str().to_owned()).await.unwrap();
        assert_eq!(target.as_str(), "https://example.com");
        let after = svc.get(link.code.as_str().to_owned()).await.unwrap();
        assert_eq!(after.hits, 1);
    }

    #[tokio::test]
    async fn missing_code_is_not_found() {
        let svc = service();
        assert!(matches!(
            svc.get("missing".to_owned()).await,
            Err(ServiceError::NotFound)
        ));
        assert!(matches!(
            svc.resolve("missing".to_owned()).await,
            Err(ServiceError::NotFound)
        ));
        assert!(matches!(
            svc.delete("missing".to_owned()).await,
            Err(ServiceError::NotFound)
        ));
    }

    #[tokio::test]
    async fn ready_succeeds_with_reachable_store() {
        assert!(service().ready().await.is_ok());
    }

    #[tokio::test]
    async fn delete_removes_link() {
        let svc = service();
        svc.create("https://a.com".to_owned(), Some("gone".to_owned()), None)
            .await
            .unwrap();
        svc.delete("gone".to_owned()).await.unwrap();
        assert!(matches!(
            svc.get("gone".to_owned()).await,
            Err(ServiceError::NotFound)
        ));
    }

    #[tokio::test]
    async fn create_with_ttl_sets_future_expiry_and_resolves() {
        let svc = service();
        let link = svc
            .create("https://example.com".to_owned(), Some("ttl".to_owned()), Some(3600))
            .await
            .unwrap();
        assert!(link.expires_at.is_some());
        // Not expired yet, so it still resolves.
        assert!(svc.resolve("ttl".to_owned()).await.is_ok());
    }

    #[tokio::test]
    async fn blocked_host_is_rejected_subdomains_included() {
        let repo = Arc::new(InMemoryLinkRepository::default());
        let svc = LinkService::new(repo, vec!["evil.com".to_owned()]);

        // Exact host and a subdomain are both blocked.
        assert!(matches!(
            svc.create("https://evil.com/x".to_owned(), None, None).await,
            Err(ServiceError::Blocked)
        ));
        assert!(matches!(
            svc.create("https://sub.evil.com/x".to_owned(), None, None).await,
            Err(ServiceError::Blocked)
        ));
        // A different host is allowed (note: "notevil.com" must NOT match).
        assert!(svc
            .create("https://notevil.com".to_owned(), None, None)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn expired_link_is_not_found_and_purged() {
        let repo = Arc::new(InMemoryLinkRepository::default());
        let svc = LinkService::new(repo.clone(), Vec::new());

        // Insert a link that already expired (far in the past), directly via the repo.
        let expired = Link::with_expiry(
            ShortCode::parse("old").unwrap(),
            TargetUrl::parse("https://example.com").unwrap(),
            1_000,
            Some(1_001),
        );
        repo.insert(&expired).await.unwrap();

        assert!(matches!(
            svc.get("old".to_owned()).await,
            Err(ServiceError::NotFound)
        ));
        // Lazy purge removed it from the store.
        let gone = repo.get(&ShortCode::parse("old").unwrap()).await.unwrap();
        assert!(gone.is_none());
    }
}
