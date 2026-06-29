# Pastebin Service

A clean, layered pastebin REST API (Axum + SQLite/sqlx), sharing the URL
shortener's hexagonal architecture. Stores text snippets ("pastes") and serves
them back by id, with planned expiry (TTL), burn-after-read, and size limits.

> Status: **PR #1 (scaffold)** — config, error type, `GET /health`, graceful
> shutdown, and a health integration test. Roadmap and per-PR test cases:
> [`../docs/PR_PLAN_pastebin.md`](../docs/PR_PLAN_pastebin.md). Architecture:
> [`../docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md).

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
curl localhost:8090/health  # {"status":"ok"}
```

## Quality gates

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

`#![forbid(unsafe_code)]`; config injected (no globals); request bodies size
limited; graceful shutdown. Part of the repo-root Cargo workspace.
