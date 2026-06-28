//! Infrastructure layer: concrete adapters implementing domain ports.
//!
//! - [`InMemoryLinkRepository`] — a thread-safe in-memory implementation used
//!   as a test double and for local runs (PR #3).
//! - `SqliteLinkRepository` — the production sqlx adapter (PR #4).

mod memory;

pub use memory::InMemoryLinkRepository;
