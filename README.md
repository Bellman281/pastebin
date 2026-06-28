# Rust REST API Workspace

Two independent Rust REST API services, each in its own folder. Both follow the
same clean, layered (hexagonal) architecture and the same engineering bar:
**SOLID, memory-efficient, and free of leaks**.

| Folder | Service | Status |
|---|---|---|
| [`url-shortener/`](./url-shortener) | Shorten long URLs to short codes and redirect | **In progress** |
| [`pastebin-service/`](./pastebin-service) | Create and fetch text snippets ("pastes") | Planned |

## Stack (both services)

- **Language:** Rust (2021 edition), `async`/`await`
- **Web framework:** [Axum](https://github.com/tokio-rs/axum) on the Tokio runtime
- **Storage:** SQLite via [`sqlx`](https://github.com/launchbadge/sqlx) (compile-time checked queries, async, connection pool)
- **API style:** JSON REST

## Shared engineering principles

See [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) for the full rationale. In short:

- **Layered/hexagonal:** `domain` (pure logic + ports) → `application` (use cases) → `infrastructure` (sqlx adapters) → `api` (Axum). Dependencies point inward only.
- **SOLID:** storage is a trait (`Repository`) so handlers depend on an abstraction, not SQLite.
- **Memory:** shared state is held behind `Arc`, never cloned wholesale; a bounded `sqlx` pool caps connections; no `Box::leak`, no reference cycles (`Rc`/`Arc` cycles avoided).
- **Safety:** `#![forbid(unsafe_code)]` in every crate.

## Build & test

```bash
# from a service folder, e.g. url-shortener/
cargo build
cargo test
cargo clippy -- -D warnings
cargo run
```

## Review order

Per the agreed plan we ship **many small PRs**. Work proceeds on the URL
shortener first; see [`docs/PR_PLAN_url_shortener.md`](./docs/PR_PLAN_url_shortener.md),
then [`docs/PR_PLAN_pastebin.md`](./docs/PR_PLAN_pastebin.md).
