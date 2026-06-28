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

        Ok(Self {
            bind_addr,
            max_body_bytes,
            database_url,
            database_max_connections,
            public_base_url,
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
