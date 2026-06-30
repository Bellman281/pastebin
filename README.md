# Two Independent Rust Services

This repository holds **two completely separate projects**. Each is fully
self-contained in its own folder — its own `Cargo.toml`, build, tests, Docker
setup, docs, and `LICENSE`. They share **no code and no database**, and there is
**no Cargo workspace** tying them together: `cd` into either folder and it builds
and runs on its own. **Delete either folder and the other is unaffected.**

| Folder | Project | Port | README |
|---|---|---|---|
| [`url-shortener/`](./url-shortener) | URL shortener REST API (Axum + SQLite) — short codes, redirects, TTL, host blocklist, rate limiting, optional Redis cache. | 8080 | [README](./url-shortener/README.md) |
| [`pastebin-service/`](./pastebin-service) | Zero-knowledge pastebin (Axum + SQLite) — browser AES-256-GCM, key in URL fragment, optional password, TTL, burn-after-read. | 8090 | [README](./pastebin-service/README.md) |

Each builds and tests standalone:

```bash
cd url-shortener    && cargo test && cargo run     # serves on :8080
cd pastebin-service && cargo test && cargo run     # serves on :8090
```

Each folder also ships its own `Dockerfile` + `docker-compose.yml` and a `docs/`
directory (architecture and, for the shortener, design/scaling notes).

The only necessarily-shared file is CI:
[`.github/workflows/ci.yml`](./.github/workflows/ci.yml) — GitHub only reads
workflows from the repo root, so it builds each service independently via a
matrix (one job per folder). To remove a service, delete its folder and its
matrix entry.
