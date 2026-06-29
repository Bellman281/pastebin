//! Strongly-typed configuration loaded from the environment.
//!
//! Read once at startup and injected (never a global). Grows per PR: the SQLite
//! URL/pool size arrive with the storage adapter, the content size limit with
//! the domain.

use std::net::SocketAddr;

/// Runtime configuration for the service.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Maximum accepted request body size, in bytes.
    pub max_body_bytes: usize,
}

impl Config {
    /// Build configuration from environment variables, falling back to
    /// development-friendly defaults. Errors only when a provided value is
    /// present but unparseable.
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind_addr = env_or("APP_BIND_ADDR", "127.0.0.1:8090")
            .parse()
            .map_err(|_| ConfigError::Invalid("APP_BIND_ADDR"))?;

        let max_body_bytes = env_or("MAX_BODY_BYTES", "1048576")
            .parse()
            .map_err(|_| ConfigError::Invalid("MAX_BODY_BYTES"))?;

        Ok(Self { bind_addr, max_body_bytes })
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
