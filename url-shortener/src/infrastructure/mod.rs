//! Infrastructure layer: concrete adapters implementing domain ports.
//!
//! - [`InMemoryLinkRepository`] — an actor-based in-memory implementation (a
//!   task owns the map; callers message it over a channel — no lock) used as a
//!   test double and for local runs (PR #3).
//! - `SqliteLinkRepository` — the production sqlx adapter (PR #4).

mod memory;
mod sqlite;

pub use memory::InMemoryLinkRepository;
pub use sqlite::SqliteLinkRepository;
