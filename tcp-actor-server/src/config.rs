//! Strongly-typed configuration loaded from the environment.
//!
//! Read once at startup and injected (never a global), keeping the dependency
//! graph explicit and testable.

use std::net::SocketAddr;
use std::time::Duration;

/// Runtime configuration for the server.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the TCP server binds to.
    pub bind_addr: SocketAddr,
    /// Upper bound on concurrently-handled connections. The accept loop holds a
    /// semaphore permit per connection, so at capacity it stops accepting
    /// (backpressure) instead of spawning unbounded tasks.
    pub max_connections: usize,
    /// Maximum accepted request-body size, in bytes (larger → `413`).
    pub max_body_bytes: usize,
    /// Per-read inactivity timeout. An idle keep-alive connection is closed after
    /// this; a stalled in-flight request gets a `408`.
    pub request_timeout: Duration,
    /// How long to let in-flight connections drain after a shutdown signal.
    pub shutdown_grace: Duration,
}

impl Config {
    /// Build configuration from environment variables, falling back to
    /// development-friendly defaults. Errors only when a provided value is
    /// present but unparseable.
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind_addr = env_or("APP_BIND_ADDR", "127.0.0.1:8080")
            .parse()
            .map_err(|_| ConfigError::Invalid("APP_BIND_ADDR"))?;

        let max_connections = env_or("MAX_CONNECTIONS", "10000")
            .parse()
            .map_err(|_| ConfigError::Invalid("MAX_CONNECTIONS"))?;

        let max_body_bytes = env_or("MAX_BODY_BYTES", "65536")
            .parse()
            .map_err(|_| ConfigError::Invalid("MAX_BODY_BYTES"))?;

        let request_timeout_secs: u64 = env_or("REQUEST_TIMEOUT_SECS", "30")
            .parse()
            .map_err(|_| ConfigError::Invalid("REQUEST_TIMEOUT_SECS"))?;

        let shutdown_grace_secs: u64 = env_or("SHUTDOWN_GRACE_SECS", "10")
            .parse()
            .map_err(|_| ConfigError::Invalid("SHUTDOWN_GRACE_SECS"))?;

        Ok(Self {
            bind_addr,
            max_connections,
            max_body_bytes,
            request_timeout: Duration::from_secs(request_timeout_secs),
            shutdown_grace: Duration::from_secs(shutdown_grace_secs),
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
