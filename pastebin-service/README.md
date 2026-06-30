# Pastebin Service

A clean, layered pastebin REST API (Axum + SQLite/sqlx), sharing the URL
shortener's hexagonal architecture. Stores text snippets ("pastes") and serves
them back by id, with planned expiry (TTL), burn-after-read, and size limits.

> Status: **functional through PR #5** — create / fetch / raw / delete over
> SQLite, with TTL expiry, burn-after-read, and content size limits. Roadmap and
> per-PR test cases: [`../docs/PR_PLAN_pastebin.md`](../docs/PR_PLAN_pastebin.md).
> Architecture: [`../docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md).

## Zero-knowledge web client

The server also serves a tiny browser client (`GET /`, `GET /app.js`) that
encrypts and decrypts **in the browser** with AES-256-GCM (WebCrypto). The
random key is base64url-encoded into the link's `#fragment`, which browsers
never send to the server — so the server only ever stores ciphertext and has
**zero knowledge** of the plaintext or key. Burn-after-read and TTL apply as
usual. Share links look like `https://host/#<id>.<key>`.

Limitations to know: you must trust the server to serve honest JS; there's no
password/PBKDF2 layer (the key is the URL fragment); use HTTPS in production.

## Planned endpoints

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/api/pastes` | Create a paste (`{"content","syntax?","ttl_seconds?","one_shot?"}`) |
| `GET` | `/api/pastes/:id` | Fetch metadata + content (JSON) |
| `GET` | `/raw/:id` | Raw content as `text/plain` |
| `DELETE` | `/api/pastes/:id` | Delete a paste |
| `GET` | `/health` · `/health/ready` | Liveness · readiness |

## Run

```bash
cp .env.example .env        # optional; defaults work out of the box
cargo run                   # serves on 127.0.0.1:8090
```

```bash
# Create a paste (optionally: "syntax", "ttl_seconds", "one_shot": true)
curl -s -X POST http://127.0.0.1:8090/api/pastes \
  -H 'Content-Type: application/json' \
  -d '{"content":"hello world","syntax":"text"}'
# -> 201 {"id":"Ab3xY7q2","url":"http://127.0.0.1:8090/api/pastes/Ab3xY7q2",...}

curl http://127.0.0.1:8090/api/pastes/Ab3xY7q2   # JSON metadata + content
curl http://127.0.0.1:8090/raw/Ab3xY7q2          # raw text/plain
curl -X DELETE http://127.0.0.1:8090/api/pastes/Ab3xY7q2   # 204
```

A `one_shot` paste is deleted on its first fetch (burn-after-read); a paste with
`ttl_seconds` becomes `404` once expired.

## Hardening

Every request passes through a middleware stack (outer → inner): `tower_http`
TraceLayer (per-request logs — `RUST_LOG=info,tower_http=debug`), CatchPanicLayer
(panic → `500`), TimeoutLayer (`REQUEST_TIMEOUT_SECS` → `408`),
ConcurrencyLimitLayer (`MAX_CONCURRENT_REQUESTS`), and the body-size limit.
`/health` is dependency-free liveness; `/health/ready` checks the DB (`503` when
unreachable). Shutdown is graceful so the pool drains.

**Per-IP rate limiting** (opt-in): set `RATE_LIMIT_RPS` > 0 (with optional
`RATE_LIMIT_BURST`) to cap requests per client IP via an in-process token
bucket; over-limit requests get `429`. Per-instance limit (each replica counts
independently).

**Redis read-cache** (optional): set `REDIS_URL` to cache fetched pastes, so hot
reads skip the database. Best-effort with DB fallback when Redis is
unset/unreachable. **One-shot pastes are never cached** (so burn-after-read
stays correct); cached entries carry a bounded TTL (≤ the paste's expiry) and are
invalidated on delete.

## Quality gates

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

`#![forbid(unsafe_code)]`; config injected (no globals); request bodies size
limited; graceful shutdown. Part of the repo-root Cargo workspace.
