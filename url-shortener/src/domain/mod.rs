//! Domain layer: pure entities, validation, and the repository port.
//!
//! This module has no dependency on Axum, sqlx, or HTTP. It defines *what* a
//! link is and *what* persistence operations exist (the `LinkRepository`
//! trait), but not *how* they are served or stored.

use std::fmt;

/// A boxed, thread-safe error used to carry an opaque storage failure cause
/// across the port boundary without coupling the domain to any backend.
pub type BoxedError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Minimum length of a short code (in characters).
pub const CODE_MIN_LEN: usize = 1;
/// Maximum length of a short code (in characters).
pub const CODE_MAX_LEN: usize = 32;
/// Maximum accepted length of a target URL (in bytes).
pub const URL_MAX_LEN: usize = 2048;

/// Errors produced when validating client-supplied values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("short code must be between 1 and 32 characters")]
    CodeLength,
    #[error("short code must contain only ASCII letters and digits")]
    CodeCharset,
    #[error("url must be 2048 bytes or fewer")]
    UrlTooLong,
    #[error("url must be a valid absolute URL with a host")]
    UrlInvalid,
    #[error("url scheme must be http or https")]
    UrlScheme,
}

/// A validated short code: 1–32 ASCII alphanumeric characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShortCode(String);

impl ShortCode {
    /// Validate and construct a short code from untrusted input.
    pub fn parse(raw: impl Into<String>) -> Result<Self, ValidationError> {
        let value = raw.into();
        if value.len() < CODE_MIN_LEN || value.len() > CODE_MAX_LEN {
            return Err(ValidationError::CodeLength);
        }
        if !value.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(ValidationError::CodeCharset);
        }
        Ok(Self(value))
    }

    /// Construct from a value already known to be valid (e.g. a generated code
    /// or a row loaded from storage). Skips revalidation; callers must uphold
    /// the invariant.
    pub fn from_trusted(value: String) -> Self {
        debug_assert!(
            value.len() >= CODE_MIN_LEN
                && value.len() <= CODE_MAX_LEN
                && value.bytes().all(|b| b.is_ascii_alphanumeric()),
            "from_trusted called with an invalid short code"
        );
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ShortCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A validated target URL: an absolute `http`/`https` URL with a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetUrl(String);

impl TargetUrl {
    /// Validate and construct a target URL from untrusted input.
    pub fn parse(raw: impl Into<String>) -> Result<Self, ValidationError> {
        let value = raw.into();
        if value.len() > URL_MAX_LEN {
            return Err(ValidationError::UrlTooLong);
        }
        let parsed = url::Url::parse(&value).map_err(|_| ValidationError::UrlInvalid)?;
        match parsed.scheme() {
            "http" | "https" => {}
            _ => return Err(ValidationError::UrlScheme),
        }
        if !parsed.has_host() {
            return Err(ValidationError::UrlInvalid);
        }
        Ok(Self(value))
    }

    /// Construct from a value already known to be valid (e.g. loaded from
    /// storage). Skips revalidation.
    pub fn from_trusted(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TargetUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A shortened link and its metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub code: ShortCode,
    pub target: TargetUrl,
    /// Creation time as Unix seconds (UTC).
    pub created_at: i64,
    /// Number of successful redirects served.
    pub hits: i64,
}

impl Link {
    /// Create a brand-new link with zero hits.
    pub fn new(code: ShortCode, target: TargetUrl, created_at: i64) -> Self {
        Self { code, target, created_at, hits: 0 }
    }
}

/// Errors a [`LinkRepository`] can return.
#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    /// The short code already exists (unique-constraint violation on insert).
    #[error("short code already exists")]
    Conflict,
    /// An opaque backend failure (I/O, driver, etc.).
    #[error(transparent)]
    Backend(BoxedError),
}

/// Persistence port for links. Implementations live in `infrastructure`.
///
/// The trait is object-safe (via `async_trait`) so the application can depend
/// on `Arc<dyn LinkRepository>` — an abstraction, not a concrete database.
#[async_trait::async_trait]
pub trait LinkRepository: Send + Sync + 'static {
    /// Persist a new link. Returns [`RepoError::Conflict`] if the code is taken.
    async fn insert(&self, link: &Link) -> Result<(), RepoError>;

    /// Fetch a link by code, or `None` if it does not exist.
    async fn get(&self, code: &ShortCode) -> Result<Option<Link>, RepoError>;

    /// Increment the hit counter; returns `true` if a row was updated.
    async fn increment_hits(&self, code: &ShortCode) -> Result<bool, RepoError>;

    /// Delete a link; returns `true` if a row was removed.
    async fn delete(&self, code: &ShortCode) -> Result<bool, RepoError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_code_accepts_valid() {
        assert_eq!(ShortCode::parse("abc123").unwrap().as_str(), "abc123");
    }

    #[test]
    fn short_code_rejects_empty_and_overlong() {
        assert_eq!(ShortCode::parse(""), Err(ValidationError::CodeLength));
        let too_long = "a".repeat(CODE_MAX_LEN + 1);
        assert_eq!(ShortCode::parse(too_long), Err(ValidationError::CodeLength));
    }

    #[test]
    fn short_code_rejects_bad_charset() {
        assert_eq!(ShortCode::parse("ab-c"), Err(ValidationError::CodeCharset));
        assert_eq!(ShortCode::parse("a/b"), Err(ValidationError::CodeCharset));
    }

    #[test]
    fn target_url_accepts_http_and_https() {
        assert!(TargetUrl::parse("https://example.com/path?q=1").is_ok());
        assert!(TargetUrl::parse("http://example.com").is_ok());
    }

    #[test]
    fn target_url_rejects_other_schemes_and_garbage() {
        assert_eq!(
            TargetUrl::parse("ftp://example.com"),
            Err(ValidationError::UrlScheme)
        );
        assert_eq!(
            TargetUrl::parse("not a url"),
            Err(ValidationError::UrlInvalid)
        );
    }

    #[test]
    fn target_url_rejects_overlong() {
        let long = format!("https://example.com/{}", "x".repeat(URL_MAX_LEN));
        assert_eq!(TargetUrl::parse(long), Err(ValidationError::UrlTooLong));
    }

    #[test]
    fn new_link_starts_with_zero_hits() {
        let link = Link::new(
            ShortCode::parse("abc").unwrap(),
            TargetUrl::parse("https://example.com").unwrap(),
            1_700_000_000,
        );
        assert_eq!(link.hits, 0);
    }
}
