//! Infrastructure layer: concrete adapters implementing domain ports.
//!
//! - [`InMemoryPasteRepository`] — an actor-based in-memory store (a task owns
//!   the map; callers message it over a channel — no lock), used as a test
//!   double / for local runs, PR #3.
//! - `SqlitePasteRepository` — the production sqlx adapter (PR #4).

mod memory;
mod sqlite;

pub use memory::InMemoryPasteRepository;
pub use sqlite::SqlitePasteRepository;
