# Pastebin Service

A clean, layered pastebin REST API (Axum + SQLite/sqlx), sharing the URL
shortener's hexagonal architecture. Stores text snippets ("pastes") and serves
them back by id, with planned expiry (TTL), burn-after-read, and size limits.

> Status: **functional through PR #5** — create / fetch / raw / delete over
> SQLite, with TTL expiry, burn-after-read, and content size limits. Roadmap and
> per-PR test cases: [`../docs/PR_PLAN_pastebin.md`](../docs/PR_PLAN_pastebin.md).
> Architecture: [`../docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md).

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

## Quality gates

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

`#![forbid(unsafe_code)]`; config injected (no globals); request bodies size
limited; graceful shutdown. Part of the repo-root Cargo workspace.
