# URL Shortener

A clean, layered URL shortener REST API built with **Axum** and **SQLite/sqlx**.

> Status: **functional through PR #5** — create, redirect (302), fetch metadata,
> and delete are all working over SQLite. Hardening (timeouts, rate limiting,
> DB-backed readiness) is PR #6. See
> [`../docs/PR_PLAN_url_shortener.md`](../docs/PR_PLAN_url_shortener.md).

## Endpoints

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/api/links` | Create a short link (`{"url": "...", "alias": "optional"}`) |
| `GET` | `/:code` | 302 redirect to the original URL (counts a hit) |
| `GET` | `/api/links/:code` | Link metadata (target, hits, created_at) |
| `DELETE` | `/api/links/:code` | Remove a link |
| `GET` | `/health` | Liveness probe |

## Layout

```
src/
  main.rs          Composition root (config -> repo -> app -> serve)
  lib.rs           AppState + build_app(); injects the repository
  config.rs        Env-driven Config
  error.rs         AppError -> IntoResponse (+ unit tests)
  domain/          Link, ShortCode/TargetUrl validation, LinkRepository port
  application/     LinkService use cases + ServiceError
  infrastructure/  InMemoryLinkRepository + SqliteLinkRepository
  api/             Axum router, handlers, DTOs
tests/
  health.rs        /health integration test
  links.rs         End-to-end link lifecycle (in-memory repo)
```

Architecture rationale: [`../docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md).

## Run

```bash
cp .env.example .env        # optional; defaults work out of the box
cargo run
```

```bash
# Create
curl -X POST http://127.0.0.1:8080/api/links \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com"}'
# -> 201 {"code":"Ab3xY7q","short_url":"http://127.0.0.1:8080/Ab3xY7q",...}

# Follow the redirect (use -L to actually go there)
curl -i http://127.0.0.1:8080/Ab3xY7q          # 302 + Location header

# Metadata
curl http://127.0.0.1:8080/api/links/Ab3xY7q   # {"code":...,"hits":1,...}

# Delete
curl -X DELETE http://127.0.0.1:8080/api/links/Ab3xY7q   # 204
```

## Quality gates

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

`#![forbid(unsafe_code)]`; config injected (no globals); the repository is
injected as `Arc<dyn LinkRepository>` (swap SQLite for in-memory without
touching handlers); the SQLite pool is bounded; request bodies are size limited;
shutdown is graceful so the pool drains without leaks.
