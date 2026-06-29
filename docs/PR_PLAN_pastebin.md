# Pastebin Service — Test-Driven PR Plan

A pastebin REST API: store text snippets ("pastes") and serve them back by id.
Same stack and hexagonal architecture as the URL shortener (Axum + SQLite/sqlx,
`domain → application → infrastructure → api`, dependencies inward only). See
[`ARCHITECTURE.md`](./ARCHITECTURE.md).

**Workflow (strict):** every PR is a branch with its **own tests**. Nothing
merges to `main` until `cargo test` + `cargo clippy -- -D warnings` pass locally.
Tests are written *with* the code they cover (each PR is green on its own).

## Service scope

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/api/pastes` | Create a paste (`{"content","syntax?","ttl_seconds?","one_shot?"}`) → `201` |
| `GET` | `/api/pastes/:id` | Fetch metadata + content (JSON) |
| `GET` | `/raw/:id` | Fetch raw content as `text/plain` |
| `DELETE` | `/api/pastes/:id` | Delete a paste → `204` |
| `GET` | `/health` · `/health/ready` | Liveness · readiness |

Paste-specific features: **expiry (TTL)**, **burn-after-read** (`one_shot`:
deleted on first view), and a **content size limit**.

---

## PR #1 — Scaffold & health
**Goal:** compilable skeleton, layered modules, `/health`, added to the workspace.
**Test cases:**
- `health_returns_ok` (integration) — `GET /health` → `200 {"status":"ok"}`.
**Done:** `cargo run` serves `/health`; test green; crate builds in the workspace.

## PR #2 — Domain & port
**Goal:** pure entities + validation + the repository port (no IO).
**Test cases (unit):**
- `paste_id_accepts_valid` / `rejects_empty_overlong` / `rejects_bad_charset`.
- `content_rejects_empty` and `content_rejects_oversized` (> `MAX_CONTENT_BYTES`).
- `is_expired_respects_the_boundary` (now == expiry ⇒ expired).
- `new_paste_starts_with_zero_views_and_no_expiry`.
**Deliverables:** `Paste`, `PasteId`, `Content` newtypes, `ValidationError`,
`PasteRepository` trait (`insert`/`get`/`delete`/`ping`), `RepoError`.

## PR #3 — Application + in-memory repo
**Goal:** use cases against the port; testable without a DB.
**Test cases (unit, in-memory double):**
- `create_then_fetch_roundtrips` (content + syntax preserved).
- `create_rejects_empty_and_oversized_content`.
- `fetch_expired_is_not_found_and_purged`.
- `one_shot_paste_is_burned_after_first_fetch` (1st fetch OK, 2nd ⇒ NotFound).
- `non_one_shot_fetch_increments_views`.
- `missing_id_is_not_found`; `delete_removes_paste`.
**Deliverables:** `PasteService` (`create`/`fetch`/`delete`/`ready`), id
generation (random base62), `InMemoryPasteRepository`.

## PR #4 — SQLite adapter
**Goal:** persistence implementing the same port.
**Test cases (in-memory SQLite, `max_connections = 1`):**
- `insert_get_delete_roundtrip`; `duplicate_id_conflicts`; `ping_ok`.
- nullable `expires_at` and `one_shot` flag round-trip correctly.
**Deliverables:** `migrations/0001_init.sql`, `SqlitePasteRepository`
(runtime queries, unique-violation ⇒ `Conflict`, WAL + busy_timeout).

## PR #5 — HTTP API
**Goal:** expose use cases over HTTP.
**Test cases (end-to-end, in-memory repo):**
- `create_then_fetch_then_raw_then_delete` lifecycle; `raw` returns
  `text/plain` with the exact bytes.
- `oversized_content_is_rejected` (`413`/`400`); `empty_content_is_400`.
- `ttl_paste_expires` (ttl 0 ⇒ fetch `404`).
- `one_shot_burns_over_http` (2nd `GET` ⇒ `404`).
- `unknown_id_is_404`.
**Deliverables:** DTOs, handlers, `ServiceError → AppError` map, `main` wires the
SQLite repo, `/health/ready`.

## PR #6 — Hardening
**Goal:** production manners (reuse the shortener's stack).
**Test cases:** `oversized_body_413`, `method_not_allowed_405`.
**Deliverables:** Trace/CatchPanic/Timeout/ConcurrencyLimit layers, DB-backed
readiness, graceful shutdown.

---

## Cross-cutting acceptance criteria (every PR)
- `#![forbid(unsafe_code)]`; no `unwrap`/`panic` on the request path.
- New behaviour has tests; existing tests stay green.
- `cargo clippy -- -D warnings` and `cargo fmt --check` clean.
- Branch is **not merged** until the above pass locally.
