//! Strongly-typed configuration loaded from the environment.
//!
//! Config is read once at startup and then injected (never accessed as a
//! global), keeping the dependency graph explicit and testable.

use std::net::SocketAddr;

/// Runtime configuration for the service.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Maximum accepted request body size, in bytes.
    pub max_body_bytes: usize,
    /// sqlx connection URL (e.g. `sqlite://data/links.db`).
    pub database_url: String,
    /// Upper bound on pooled DB connections — caps connection memory under load.
    pub database_max_connections: u32,
    /// Base URL used to build the full short link returned to clients.
    pub public_base_url: String,
    /// Per-request timeout in seconds (a slow request is cut off with 408).
    pub request_timeout_secs: u64,
    /// Max in-flight requests; excess requests wait (bounded by the timeout).
    pub max_concurrent_requests: usize,
    /// Hosts (and their subdomains) that may not be shortened. Lowercased.
    pub blocked_hosts: Vec<String>,
    /// Per-IP rate limit in requests/second. `0` disables rate limiting.
    pub rate_limit_rps: u32,
    /// Per-IP burst capacity. When `0` and `rate_limit_rps > 0`, defaults to rps.
    pub rate_limit_burst: u32,
}

impl Config {
    /// Build configuration from environment variables, falling back to
    /// development-friendly defaults. Returns an error only when a provided
    /// value is present but unparseable.
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind_addr = env_or("APP_BIND_ADDR", "127.0.0.1:8080")
            .parse()
            .map_err(|_| ConfigError::Invalid("APP_BIND_ADDR"))?;

        let max_body_bytes = env_or("MAX_BODY_BYTES", "16384")
            .parse()
            .map_err(|_| ConfigError::Invalid("MAX_BODY_BYTES"))?;

        let database_url = env_or("DATABASE_URL", "sqlite://links.db");

        let database_max_connections = env_or("DATABASE_MAX_CONNECTIONS", "5")
            .parse()
            .map_err(|_| ConfigError::Invalid("DATABASE_MAX_CONNECTIONS"))?;

        let public_base_url = env_or("PUBLIC_BASE_URL", "http://127.0.0.1:8080");

        let request_timeout_secs = env_or("REQUEST_TIMEOUT_SECS", "10")
            .parse()
            .map_err(|_| ConfigError::Invalid("REQUEST_TIMEOUT_SECS"))?;

        let max_concurrent_requests = env_or("MAX_CONCURRENT_REQUESTS", "1024")
            .parse()
            .map_err(|_| ConfigError::Invalid("MAX_CONCURRENT_REQUESTS"))?;

        // Comma-separated host denylist, e.g. "evil.com, malware.test".
        let blocked_hosts = env_or("BLOCKED_HOSTS", "")
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();

        let rate_limit_rps = env_or("RATE_LIMIT_RPS", "0")
            .parse()
            .map_err(|_| ConfigError::Invalid("RATE_LIMIT_RPS"))?;

        let rate_limit_burst = env_or("RATE_LIMIT_BURST", "0")
            .parse()
            .map_err(|_| ConfigError::Invalid("RATE_LIMIT_BURST"))?;

        Ok(Self {
            bind_addr,
            max_body_bytes,
            database_url,
            database_max_connections,
            public_base_url,
            request_timeout_secs,
            max_concurrent_requests,
            blocked_hosts,
            rate_limit_rps,
            rate_limit_burst,
        })
    }
}

/// Errors that can occur while reading configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid value for {0}")]
    Invalid(&'static str),
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}
