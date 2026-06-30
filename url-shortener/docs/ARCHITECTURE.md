# Architecture & Engineering Conventions

Both services share this layout and these rules. The goal is code that is
**clean, SOLID, memory-efficient, and leak-free** by construction.

## Layered (hexagonal) structure

```
src/
  main.rs            Composition root: load config, build pool, wire deps, serve.
  lib.rs             Re-exports; builds the Axum Router from injected deps.
  config.rs          Strongly-typed config loaded from env (no globals).
  error.rs           Single AppError enum -> IntoResponse mapping.
  domain/            Entities + the ports (traits). Zero framework/IO deps.
  application/       Use cases / services. Orchestrate domain + ports.
  infrastructure/    Adapters: sqlx repositories implementing domain ports.
  api/               Axum routers, handlers, DTOs. Maps HTTP <-> use cases.
```

**Dependency rule:** arrows point inward only. `domain` knows nothing about
Axum or sqlx. `api` and `infrastructure` depend on `domain`, never the reverse.
This is the Dependency Inversion Principle made physical.

## SOLID mapping

- **S**ingle responsibility: handlers parse/format HTTP only; use cases hold
  business rules; repositories do persistence only.
- **O**pen/closed: new storage backends are added by implementing the
  `Repository` trait — no edits to use cases.
- **L**iskov: any `Repository` impl (SQLite, in-memory test double) is
  substitutable; tests use an in-memory double.
- **I**nterface segregation: small, focused traits (e.g. `LinkRepository`)
  rather than one fat "DAO".
- **D**ependency inversion: use cases depend on the trait; `main.rs` injects the
  concrete sqlx adapter.

## Memory efficiency & no leaks

- Shared, read-only state (config, repo handle) lives in one `Arc` injected as
  Axum state — cloning an `Arc` bumps a refcount, it does not copy data.
- `sqlx::SqlitePool` is bounded (`max_connections`) so connection memory is
  capped under load; the pool is the single owner of DB handles.
- No `Box::leak`, no `'static` smuggling, no `std::mem::forget`.
- No `Rc`/`Arc` reference cycles (which would leak): state is a tree, not a graph.
- Request bodies are size-limited (Axum `DefaultBodyLimit`) to bound per-request
  allocation and reject abuse.
- Prefer borrowing (`&str`) over owned `String` in hot paths; allocate only at
  boundaries (deserialization, DB rows).
- `#![forbid(unsafe_code)]` guarantees no manual memory management.

## Error handling

One `AppError` enum per crate implementing `IntoResponse`, so handlers return
`Result<T, AppError>` and every failure becomes a clean JSON error with the
right status code. No `unwrap()`/`expect()` on the request path.

## Testing

- Unit tests for domain logic (e.g. short-code validation) — pure, no IO.
- Use-case tests against an in-memory `Repository` double.
- Integration tests hit the real Axum app over an in-memory SQLite DB.
- CI gate: `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`.
