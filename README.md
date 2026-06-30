# Rust REST API Monorepo — URL Shortener & Zero-Knowledge Pastebin

This repository hosts **two independent services**, each in its own folder, that
happen to share one Cargo workspace, one architecture, and one engineering bar.
They are *separate projects* — you can build, test, deploy, and reason about each
on its own.

| Folder | Service | What it does | Status |
|---|---|---|---|
| [`url-shortener/`](./url-shortener) | **URL shortener** | Turn a long URL into a short code and redirect to it; tracks hits, supports custom aliases, TTL expiry, and a host blocklist. | ✅ complete |
| [`pastebin-service/`](./pastebin-service) | **Pastebin** | Store text snippets and serve them back; TTL expiry, burn-after-read, and a **zero-knowledge** browser client (AES-256-GCM). | ✅ complete |

They are deliberately **not** coupled: no shared application code, no shared
database, no cross-calls. The workspace exists only to share a build cache and
dependency versions. Each folder has its own `README`, `Cargo.toml`, migrations,
tests, and `.env.example`.

---

## Shared stack

- **Language:** Rust (2021 edition), `async`/`await` on the Tokio runtime.
- **Web framework:** [Axum](https://github.com/tokio-rs/axum) 0.7.
- **Storage:** SQLite via [`sqlx`](https://github.com/launchbadge/sqlx) (async, bounded pool, WAL mode).
- **Optional:** [Redis](https://redis.io/) read-cache, per-IP rate limiting (both opt-in).
- **API style:** JSON REST.

## Architecture (read this first if you're new)

Both services use a **hexagonal (ports & adapters)** layout. The rule is simple:
**dependencies point inward only**, so the business logic never knows about HTTP
or SQL.

```
            HTTP            use cases             port              adapter
client ──▶  api   ───────▶  application  ───────▶ domain   ◀─────── infrastructure
          (Axum)         (Service: the         (Repository       (SQLite / in-memory /
                          business rules)        trait = port)     Redis cache)
                                  │                                       │
                                  └───────────── depends on ──────────────┘
                                            (the trait, not the DB)
```

| Layer | Folder (per service) | Responsibility | Knows about |
|---|---|---|---|
| **domain** | `src/domain/` | Entities + validated newtypes + the `Repository` **trait** (the port). Pure, no I/O. | nothing external |
| **application** | `src/application/` | Use cases (`LinkService` / `PasteService`): the actual rules. | `domain` only |
| **infrastructure** | `src/infrastructure/` | Adapters that *implement* the port: SQLite (`sqlx`), an in-memory double for tests. | `domain` |
| **api** | `src/api/` | Axum router, handlers, DTOs; maps HTTP ⇄ use cases and errors → status codes. | `domain` + `application` |
| **composition** | `src/main.rs`, `src/lib.rs` | Build config, construct the concrete adapter, inject it as `Arc<dyn Repository>`, serve. | everything |

**Why it's built this way:** the `Repository` trait is the seam. Production
injects the SQLite adapter; tests inject an in-memory one; a Redis cache or a
future Postgres backend slots in as another adapter — none of it touches the
domain or application layers. That is what keeps the code testable (most tests
need no database) and swappable.

Engineering bar held across both crates:

- `#![forbid(unsafe_code)]`; no `unwrap`/`panic` on the request path.
- Shared state is a single `Arc` (pointer-clone per request, never a data copy); the `sqlx` pool is bounded; no `Box::leak`, no `Rc`/`Arc` cycles.
- Graceful shutdown drains the pool. Request bodies are size-limited.
- Every service ships hardening middleware: structured tracing, panic-catching, request timeout, concurrency cap, and split liveness/readiness probes.

Deeper rationale lives in [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md),
[`docs/DESIGN.md`](./docs/DESIGN.md) (shared-state, CAP-per-dataset, ID strategy),
and [`docs/SCALING.md`](./docs/SCALING.md).

## Build & test

It's a Cargo workspace, so from the repo root:

```bash
cargo test --all                 # ~85 tests across both services, no DB needed
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

Or per service, from its folder (`url-shortener/` or `pastebin-service/`):

```bash
cargo run     # shortener on :8080, pastebin on :8090
```

CI ([`.github/workflows/ci.yml`](./.github/workflows/ci.yml)) runs fmt + clippy +
tests on every push and PR. A multi-stage [`Dockerfile`](./Dockerfile) +
[`docker-compose.yml`](./docker-compose.yml) package the URL shortener.

## Security note — the pastebin is *zero-knowledge*

The pastebin server **never sees your plaintext or your key**. Encryption and
decryption happen entirely in the browser using **256-bit AES-GCM** (authenticated
encryption). The random key is placed in the link's URL **fragment** (`…/#<id>.<key>`),
and browsers never transmit the fragment to the server — so the server only ever
stores ciphertext and has *plausible deniability* over paste contents.

What that protects, and what it does **not** (read before trusting it with secrets):

- ✅ A server breach or a subpoena yields only ciphertext.
- ✅ Burn-after-read and TTL limit how long a secret can exist.
- ⚠️ You must **trust the server to serve honest JavaScript** — use HTTPS (ideally HSTS); a compromised server could ship code that leaks the key.
- ⚠️ Anyone with the full link (including the `#key`) can read the paste. There is **no password layer yet** (see roadmap), so share links privately.
- ⚠️ Access logs can still reveal *who* fetched a paste, even if not *what*.

Details: [`pastebin-service/README.md`](./pastebin-service/README.md).

## Ideas borrowed from / inspired by PrivateBin

Our pastebin follows the same zero-knowledge model as
[PrivateBin](https://github.com/PrivateBin/PrivateBin). Features we already share,
and ones worth adding next:

| Feature | PrivateBin | Us | Notes |
|---|---|---|---|
| Client-side AES-256-GCM, key in URL fragment | ✅ | ✅ | core zero-knowledge model |
| Expiry incl. "never" + burn-after-read | ✅ | ✅ | our `ttl_seconds` + `one_shot` |
| Plausible-deniability / admin can delete | ✅ | ✅ | server stores only ciphertext |
| **Password protection** (extra secret, not in URL) | ✅ | ⬜ | derive the key from `random_key + password` via PBKDF2 — strong next step |
| **Syntax highlighting / Markdown** rendering | ✅ | ◐ | we store a `syntax` hint; client could highlight/render |
| **QR code** for the share link | ✅ | ⬜ | small client-only addition |
| File / image upload + preview | ✅ | ⬜ | larger scope; size-limited |
| Comments / discussions | ✅ | ⬜ | out of scope for now |
| Themes / i18n | ✅ | ⬜ | cosmetic |

The highest-value additions for us are **password protection** (closes the
"anyone with the link can read it" gap) and **client-side syntax highlighting**.

## Repository map

```
url-shortener/        the URL shortener service (own README, src, migrations, tests)
pastebin-service/     the pastebin service + zero-knowledge web client (static/)
docs/                 ARCHITECTURE · DESIGN · SCALING
Dockerfile · docker-compose.yml · .github/workflows/ci.yml
Cargo.toml            virtual workspace (members = both services)
```
