//! Domain layer: pure entities, validation, and the repository port.
//!
//! No dependency on Axum, sqlx, or HTTP. Defines *what* a paste is and *what*
//! persistence operations exist (`PasteRepository`), not how they're served or
//! stored.

use std::fmt;

/// A boxed, thread-safe error carrying an opaque storage failure across the
/// port boundary without coupling the domain to any backend.
pub type BoxedError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Minimum / maximum length of a paste id (characters).
pub const ID_MIN_LEN: usize = 1;
pub const ID_MAX_LEN: usize = 32;
/// Maximum accepted paste content size, in bytes.
pub const MAX_CONTENT_BYTES: usize = 1_000_000;

/// Errors produced when validating client-supplied values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("paste id must be between 1 and 32 characters")]
    IdLength,
    #[error("paste id must contain only ASCII letters and digits")]
    IdCharset,
    #[error("paste content must not be empty")]
    ContentEmpty,
    #[error("paste content must be 1000000 bytes or fewer")]
    ContentTooLarge,
}

/// A validated paste id: 1–32 ASCII alphanumeric characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PasteId(String);

impl PasteId {
    /// Validate and construct a paste id from untrusted input.
    pub fn parse(raw: impl Into<String>) -> Result<Self, ValidationError> {
        let value = raw.into();
        if value.len() < ID_MIN_LEN || value.len() > ID_MAX_LEN {
            return Err(ValidationError::IdLength);
        }
        if !value.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(ValidationError::IdCharset);
        }
        Ok(Self(value))
    }

    /// Construct from a value already known valid (generated id or stored row).
    pub fn from_trusted(value: String) -> Self {
        debug_assert!(
            value.len() >= ID_MIN_LEN
                && value.len() <= ID_MAX_LEN
                && value.bytes().all(|b| b.is_ascii_alphanumeric()),
            "from_trusted called with an invalid paste id"
        );
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PasteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validated paste content: non-empty and within the size limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content(String);

impl Content {
    /// Validate and construct content from untrusted input.
    pub fn parse(raw: impl Into<String>) -> Result<Self, ValidationError> {
        let value = raw.into();
        if value.is_empty() {
            return Err(ValidationError::ContentEmpty);
        }
        if value.len() > MAX_CONTENT_BYTES {
            return Err(ValidationError::ContentTooLarge);
        }
        Ok(Self(value))
    }

    /// Construct from a value already known valid (loaded from storage).
    pub fn from_trusted(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A stored paste and its metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paste {
    pub id: PasteId,
    pub content: Content,
    /// Optional free-form syntax/language hint (e.g. "rust", "json").
    pub syntax: Option<String>,
    /// Creation time as Unix seconds (UTC).
    pub created_at: i64,
    /// Optional expiry as Unix seconds (UTC). `None` means it never expires.
    pub expires_at: Option<i64>,
    /// Burn-after-read: deleted on first successful fetch.
    pub one_shot: bool,
    /// Number of successful fetches served.
    pub views: i64,
}

impl Paste {
    /// Create a brand-new paste with zero views.
    pub fn new(
        id: PasteId,
        content: Content,
        syntax: Option<String>,
        created_at: i64,
        expires_at: Option<i64>,
        one_shot: bool,
    ) -> Self {
        Self { id, content, syntax, created_at, expires_at, one_shot, views: 0 }
    }

    /// True if the paste has an expiry at or before `now` (Unix seconds).
    pub fn is_expired(&self, now: i64) -> bool {
        matches!(self.expires_at, Some(exp) if now >= exp)
    }
}

/// Errors a [`PasteRepository`] can return.
#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    /// The id already exists (unique-constraint violation on insert).
    #[error("paste id already exists")]
    Conflict,
    /// An opaque backend failure (I/O, driver, etc.).
    #[error(transparent)]
    Backend(BoxedError),
}

/// Persistence port for pastes. Implementations live in `infrastructure`.
///
/// Object-safe (via `async_trait`) so the application depends on
/// `Arc<dyn PasteRepository>` — an abstraction, not a concrete database.
#[async_trait::async_trait]
pub trait PasteRepository: Send + Sync + 'static {
    /// Persist a new paste. Returns [`RepoError::Conflict`] if the id is taken.
    async fn insert(&self, paste: &Paste) -> Result<(), RepoError>;

    /// Fetch a paste by id, or `None` if it does not exist.
    async fn get(&self, id: &PasteId) -> Result<Option<Paste>, RepoError>;

    /// Increment the view counter; returns `true` if a row was updated.
    async fn increment_views(&self, id: &PasteId) -> Result<bool, RepoError>;

    /// Delete a paste; returns `true` if a row was removed.
    async fn delete(&self, id: &PasteId) -> Result<bool, RepoError>;

    /// Cheap connectivity check for readiness probes.
    async fn ping(&self) -> Result<(), RepoError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_id_accepts_valid() {
        assert_eq!(PasteId::parse("abc123").unwrap().as_str(), "abc123");
    }

    #[test]
    fn paste_id_rejects_empty_and_overlong() {
        assert_eq!(PasteId::parse(""), Err(ValidationError::IdLength));
        let long = "a".repeat(ID_MAX_LEN + 1);
        assert_eq!(PasteId::parse(long), Err(ValidationError::IdLength));
    }

    #[test]
    fn paste_id_rejects_bad_charset() {
        assert_eq!(PasteId::parse("ab-c"), Err(ValidationError::IdCharset));
        assert_eq!(PasteId::parse("a/b"), Err(ValidationError::IdCharset));
    }

    #[test]
    fn content_accepts_valid() {
        assert_eq!(Content::parse("hello").unwrap().as_str(), "hello");
    }

    #[test]
    fn content_rejects_empty() {
        assert_eq!(Content::parse(""), Err(ValidationError::ContentEmpty));
    }

    #[test]
    fn content_rejects_oversized() {
        let big = "x".repeat(MAX_CONTENT_BYTES + 1);
        assert_eq!(Content::parse(big), Err(ValidationError::ContentTooLarge));
    }

    fn sample(expires_at: Option<i64>) -> Paste {
        Paste::new(
            PasteId::parse("abc").unwrap(),
            Content::parse("body").unwrap(),
            Some("rust".to_owned()),
            1_700_000_000,
            expires_at,
            false,
        )
    }

    #[test]
    fn new_paste_starts_with_zero_views_and_no_expiry() {
        let p = sample(None);
        assert_eq!(p.views, 0);
        assert_eq!(p.expires_at, None);
        assert!(!p.is_expired(i64::MAX));
    }

    #[test]
    fn is_expired_respects_the_boundary() {
        let p = sample(Some(1_700_000_100));
        assert!(!p.is_expired(1_700_000_099));
        assert!(p.is_expired(1_700_000_100)); // at expiry == expired
        assert!(p.is_expired(1_700_000_101));
    }
}
