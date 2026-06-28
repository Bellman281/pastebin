# PR Plan — URL Shortener (Axum + SQLite/sqlx)

Strategy: **many small, independently reviewable PRs**. Each PR is green on its
own (`cargo build && cargo test && cargo clippy -- -D warnings`), adds tests for
what it introduces, and keeps the dependency rule (`api`/`infra` → `domain`).

## Service scope

A JSON REST API that shortens URLs and redirects:

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/api/links` | Create a short code for a long URL (optional custom alias) |
| `GET` | `/{code}` | 301/302 redirect to the original URL |
| `GET` | `/api/links/{code}` | Fetch link metadata (target, hits, created_at) |
| `DELETE` | `/api/links/{code}` | Remove a link |
| `GET` | `/health` | Liveness/readiness |

---

## PR #1 — Scaffold & health endpoint
**Goal:** compilable skeleton, layered modules, config, `/health`.
- `Cargo.toml` with only the deps the scaffold uses (axum, tokio, serde_json, thiserror, tracing, tracing-subscriber). Storage/middleware deps land in their own PRs.
- Root virtual workspace (`/Cargo.toml`) so both services share one build cache + lockfile.
- `#![forbid(unsafe_code)]`; only the `api` module exists yet — the `domain`/`application`/`infrastructure` layers are created in their own PRs (#2/#3/#4) rather than shipped empty.
- `config.rs` (env-driven: bind addr, DB url, body limit, base URL).
- `error.rs` (`AppError` + `IntoResponse`).
- `GET /health` returns `200 {"status":"ok"}`.
- `.gitignore`, `rustfmt.toml`, `.env.example`.
**Done when:** `cargo run` serves `/health`; integration test passes.

## PR #2 — Domain model & port
**Goal:** pure domain, zero IO.
- `Link` entity (code, target URL, created_at, hits).
- `ShortCode` newtype with validation (charset, length) — unit-tested.
- URL validation (scheme allow-list http/https, max length).
- `LinkRepository` trait (the port): `insert`, `find_by_code`, `increment_hits`, `delete`.
**Done when:** domain unit tests pass; no framework deps imported.

## PR #3 — In-memory repository + use cases
**Goal:** business logic testable without a DB.
- `application` use cases: `CreateLink`, `ResolveLink`, `GetLink`, `DeleteLink`.
- Short-code generation (base62 over a counter/random; collision retry).
- `InMemoryLinkRepository` (behind a `Mutex<HashMap>`) as a `Repository` test double.
- Use-case tests cover happy path, duplicate alias, not-found.
**Done when:** use cases fully covered against the in-memory double.

## PR #4 — SQLite adapter (sqlx)
**Goal:** real persistence implementing the same port.
- Add `sqlx` dependency here (not earlier — keeps the scaffold lean).
- `migrations/0001_init.sql` (links table, unique index on code).
- `SqliteLinkRepository` implementing `LinkRepository`.
- Bounded `SqlitePool` (`max_connections`) built in `main.rs`.
- **Pooling caveat:** `sqlite::memory:` gives each pooled connection its own
  private DB (writes won't be visible across connections). Use a file path for
  real data; for in-memory tests pin `max_connections = 1` or `cache=shared`.
- Integration test runs migrations on a single shared in-memory connection.
**Done when:** adapter passes the same behavioral tests as the in-memory double.

## PR #5 — HTTP API wiring
**Goal:** expose use cases over HTTP.
- DTOs (`CreateLinkRequest`/`Response`) with serde; map to/from domain.
- Handlers for create/resolve/get/delete; `Arc<AppState>` injected as Axum state.
- `DefaultBodyLimit` set from config; proper status codes (201/301/404/409).
- Integration tests for each route against the real app + SQLite.
**Done when:** full request lifecycle works end-to-end.

## PR #6 — Observability & hardening
**Goal:** production manners.
- `tracing` + `tower-http` TraceLayer, request IDs, timeout layer.
- Graceful shutdown (SIGINT/SIGTERM) so the pool drains — no dangling handles.
- Rate-limit / concurrency cap (tower) to bound memory under load.
- `GET /health` upgraded to check DB connectivity (readiness).
**Done when:** logs are structured; shutdown is clean; load test shows flat memory.

## PR #7 — CI, docs & polish
**Goal:** repeatable quality gate.
- GitHub Actions: fmt check, clippy `-D warnings`, test, `cargo audit`.
- README with run instructions + curl examples; OpenAPI/HTTP file optional.
- Optional: `cargo deny` for license/advisory checks.
**Done when:** CI green on a clean checkout.

---

## Cross-cutting acceptance criteria (every PR)
- `#![forbid(unsafe_code)]` holds.
- No `unwrap()`/`expect()`/`panic!` on the request path.
- No `Box::leak`, `mem::forget`, or `Arc`/`Rc` cycles.
- Clippy clean at `-D warnings`; `cargo fmt --check` clean.
- New behavior has tests; existing tests stay green.
