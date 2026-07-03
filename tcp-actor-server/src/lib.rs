//! Non-blocking TCP/HTTP server on Tokio, built with the actor model.
//!
//! Keeping the wiring in the library (not just `main.rs`) makes the whole server
//! testable in-process: a test binds to port 0, drives it over a real
//! `TcpStream`, and triggers graceful shutdown — see `tests/`.

#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod http;
pub mod metrics;
pub mod registry;
pub mod server;

pub use config::Config;
pub use error::ServerError;
pub use server::Server;
