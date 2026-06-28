# PR Plan — Pastebin Service (Axum + SQLite/sqlx)

Same conventions and quality bar as the URL shortener (see
[`ARCHITECTURE.md`](./ARCHITECTURE.md)). **Many small PRs.** Started *after* the
URL shortener so it can reuse the proven layout.

## Service scope

A JSON REST API for creating and fetching text snippets ("pastes"):

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/api/pastes` | Create a paste (content, optional TTL, optional syntax, optional one-shot) |
| `GET` | `/api/pastes/{id}` | Fetch a paste's metadata + content |
| `GET` | `/raw/{id}` | Fetch raw content as `text/plain` |
| `DELETE` | `/api/pastes/{id}` | Delete a paste |
| `GET` | `/health` | Liveness/readiness |

Notable domain features vs. the shortener: **expiry (TTL)**, **burn-after-read
(one-shot)**, and **size limits** on content.

---

## PR #1 — Scaffold & health endpoint
Mirror the shortener scaffold: `Cargo.toml`, layered modules,
`#![forbid(unsafe_code)]`, config, `error.rs`, `/health`, `.gitignore`/fmt/env.

## PR #2 — Domain model & port
- `Paste` entity (id, content, content_type/syntax, created_at, expires_at, one_shot, views).
- `PasteId` newtype + validation; content size cap enforced in the domain.
- Expiry logic (`is_expired(now)`) — unit-tested, pure.
- `PasteRepository` port: `insert`, `find_by_id`, `mark_viewed`/`delete`.

## PR #3 — In-memory repository + use cases
- Use cases: `CreatePaste`, `GetPaste` (applies expiry + burn-after-read), `DeletePaste`.
- ID generation (collision-checked).
- `InMemoryPasteRepository` double; use-case tests including expired + one-shot.

## PR #4 — SQLite adapter (sqlx)
- `migrations/0001_init.sql` (pastes table; index on `expires_at`).
- `SqlitePasteRepository`; bounded pool.
- Integration test on in-memory SQLite, same behavioral suite as the double.

## PR #5 — HTTP API wiring
- DTOs + serde; handlers for create/get/raw/delete; `Arc<AppState>`.
- `DefaultBodyLimit` sized for paste content; status codes (201/404/410-gone-on-expired).
- Integration tests per route.

## PR #6 — Expiry sweeper & hardening
- Background task (Tokio interval) to purge expired pastes so the table — and
  memory/disk — stays bounded; uses a clean cancellation token tied to shutdown.
- `tracing` + TraceLayer, timeout, concurrency cap; graceful shutdown drains pool.
- Readiness `/health` checks DB.

## PR #7 — CI, docs & polish
- Reuse the shortener's CI workflow (fmt/clippy/test/audit).
- README with curl examples (create, fetch, raw, burn-after-read demo).

---

## Cross-cutting acceptance criteria (every PR)
Identical to the URL shortener: `forbid(unsafe_code)`; no panics on the request
path; no `Box::leak`/`mem::forget`/`Arc` cycles; clippy `-D warnings` and
`fmt --check` clean; new behavior tested. Plus: the sweeper must not leak tasks —
it is spawned once and cancelled on shutdown.
