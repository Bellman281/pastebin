# URL Shortener

A production-minded URL shortener REST API in **Rust**, built on **Axum** with
**SQLite/sqlx** storage. The codebase is organised as a **hexagonal (ports &
adapters) architecture** with a strict inward-pointing dependency rule, so the
business logic is framework- and database-agnostic and fully unit-testable.

> Status: functional through PR #5, with SQLite concurrency tuning (WAL +
> `busy_timeout`) applied. Roadmap: [`../docs/PR_PLAN_url_shortener.md`](../docs/PR_PLAN_url_shortener.md).
> Scaling deep-dive: [`../docs/SCALING.md`](../docs/SCALING.md).

## Endpoints

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/api/links` | Create a short link (`{"url": "...", "alias": "optional"}`) → `201` |
| `GET` | `/:code` | **302** redirect to the original URL (counts a hit) |
| `GET` | `/api/links/:code` | Link metadata (target, hits, created_at) |
| `DELETE` | `/api/links/:code` | Remove a link → `204` |
| `GET` | `/health` | Liveness probe (process up; no dependencies) |
| `GET` | `/health/ready` | Readiness probe — checks the DB; `200` ready / `503` unavailable |

## Architecture

```
            HTTP            use cases            port            adapter
client ──▶  api  ─────────▶ application ───────▶ domain  ◀─────── infrastructure
          (Axum)          (LinkService)      (LinkRepository)   (SQLite / in-memory)
                                  │                                     │
                                  └──────────── depends on ─────────────┘
                                         (the trait, not the DB)
```

Dependency arrows point **inward only**. `domain` knows nothing about Axum or
sqlx; `application` depends on the `domain` port; `api` and `infrastructure`
depend on `domain`. Concretely:

| Layer | Module | Responsibility |
|---|---|---|
| Domain | `src/domain` | Entities (`Link`), validated newtypes (`ShortCode`, `TargetUrl`), and the `LinkRepository` **port**. Pure, no I/O. |
| Application | `src/application` | `LinkService` use cases (create / resolve / get / delete) + short-code generation. Depends only on the port. |
| Infrastructure | `src/infrastructure` | Adapters implementing the port: `SqliteLinkRepository` (production) and `InMemoryLinkRepository` (tests/dev). |
| API | `src/api` | Axum router, handlers, DTOs; maps HTTP ⇄ use cases and `ServiceError` → `AppError`. |
| Composition | `src/main.rs`, `src/lib.rs` | Build config, construct the adapter, inject it as `Arc<dyn LinkRepository>`, serve with graceful shutdown. |

**Why this matters:** swapping SQLite for PostgreSQL or Redis, or adding a cache,
is a new adapter behind the same port — the domain and application layers don't
change a line. The `LinkRepository` trait is the seam (Dependency Inversion).

### Request lifecycle

- **Create** — handler validates JSON → `LinkService::create` builds a validated
  `TargetUrl`, picks a code (custom alias or generated), and inserts via the
  port. The DB's primary key is the uniqueness arbiter.
- **Redirect** — `GET /:code` → `LinkService::resolve` looks up the target by
  primary key, counts the hit, and returns `302 Found` with a `Location` header.

## How short codes are generated

The generator lives in `src/application/mod.rs` (`generate_code`). The algorithm
is deliberately simple:

1. **Draw 7 random characters** from a 62-character base62 alphabet
   (`A–Z`, `a–z`, `0–9`) using `rand::thread_rng()`, a cryptographically strong,
   per-thread RNG. That's **62⁷ ≈ 3.5 × 10¹²** possible codes.
2. **Let the database guarantee uniqueness.** The `code` column is the table's
   `PRIMARY KEY`, so the code is *not* checked beforehand — we just `INSERT`. If
   a duplicate ever occurs the insert fails with a unique violation, and
   `create` generates a fresh code and retries (bounded to 5 attempts). At 7
   chars the per-insert clash chance is ~10⁻⁶, ≈ a dozen retries total across
   10M links — negligible.
3. **Custom aliases** are supported: send `{"alias": "my-name"}` and that string
   is validated and used verbatim; a clash returns `409 Conflict`.

The code carries **no timestamp and no sequence number**. That is intentional:

- **Unguessable / non-enumerable** — you can't walk `/1`, `/2`, … to scrape
  links or infer how many exist. The CSPRNG output has nothing to predict.
- **No shared counter** — each instance generates codes independently, with no
  coordination, which is what lets the service scale out (share-nothing).
- **Same-millisecond requests are fine** — codes don't depend on time, so two
  requests in the same instant simply draw independent values; the DB arbitrates
  any rare clash.

**Reversing a code → original URL is a key lookup, not decoding.** We store the
`code → target` row at create time; `GET /:code` reads it back by primary key
and issues the 302. There is no math to "decode" a code.

> Alternative: a monotonic counter + a keyed obfuscation (Feistel/Sqids) is the
> other common approach (collision-free by construction, non-sequential if
> obfuscated). It mainly pays off at billions of links or when you want
> minimal-length codes. Trade-offs are documented in
> [`../docs/DESIGN.md`](../docs/DESIGN.md) §4. We use random because, at this
> scale, it is simpler and needs no secret key.

## Design decisions & FAQ

**Do we cache?** Reads are extremely skewed (a few codes get most hits), so a
cache is worthwhile — but it belongs in an **external store (Redis)**, not an
in-process map, so every app instance stays stateless and horizontally
scalable. The `code → target` mapping is immutable for a link's lifetime, so it
is safe to cache and only needs invalidation on delete. *(Planned.)*

**Why random codes instead of a sequential counter?** Generated codes are random
7-char base62. A sequential counter would be shorter but (1) **enumerable** —
anyone could scan `/1`, `/2`, … to harvest every link and infer total volume,
and (2) would require a **shared atomic counter**, i.e. exactly the cross-instance
shared state we avoid. Random codes need no coordination between instances.

**How is a short code reversed to the original URL?** It isn't decoded — it's a
key lookup. We persist the `code → target` row at create time; `resolve` reads it
back by primary key (cheap, index-backed) and redirects to the stored target.

**How are duplicate *codes* prevented?** The `code` column is the table's
`PRIMARY KEY`, so the database physically cannot hold two rows with the same
code. A clashing insert returns a unique-constraint violation, mapped to
`Conflict`.

**What about duplicate *target URLs*?** We intentionally **do not** deduplicate
identical long URLs — each create is its own link with its own code, optional
alias, and independent hit count (this is how mainstream shorteners behave). To
enforce one-code-per-URL instead, add a unique index on `target` and look up
before insert; the trade-off is losing custom aliases and per-link analytics.

**Collisions when many requests arrive at the same instant?** Handled with no
shared state, because the **database is the arbiter**. Two requests that generate
the same random code both attempt to insert; the primary key lets exactly one
win, the loser receives a unique violation, and `create` retries with a fresh
code (optimistic insert + bounded retry). At 7 base62 chars the *per-insert*
collision chance is ~10⁻⁶, which works out to roughly a dozen retries total
across 10M links — negligible. Custom-alias clashes return `409 Conflict`.

**Read/write concurrency.** Two aspects:
- *Correctness:* hits are counted with `UPDATE … SET hits = hits + 1`, a single
  atomic read-modify-write — concurrent redirects never lose counts (doing the
  arithmetic in application code would create a lost-update race).
- *Throughput:* SQLite allows one writer at a time. We enable **WAL** (concurrent
  readers + one writer) plus a **`busy_timeout`** so writers wait rather than
  erroring. Heavy concurrency ultimately wants PostgreSQL (MVCC: readers never
  block writers) and/or moving hit-counting off the request path.

**Shared-state philosophy.** The service is **share-nothing**: handlers derive
everything from the request plus injected read-only dependencies, and all
mutable state lives in the datastore. `Arc<dyn LinkRepository>` is *shared
ownership of an immutable handle*, not shared mutable state — so there are no
locks on the production request path. This is precisely what lets you run many
interchangeable instances behind a load balancer.

See [`../docs/SCALING.md`](../docs/SCALING.md) for the full path to ~10M users
(external cache, channel-batched hit counting, PostgreSQL, horizontal scaling).

## Hardening (PR #6)

Every request passes through a middleware stack (outer → inner): a
`tower_http` **TraceLayer** (structured per-request logs — set
`RUST_LOG=info,tower_http=debug` to see them), a **CatchPanicLayer** (a handler
panic becomes a `500`, not a dropped connection), a **TimeoutLayer**
(`REQUEST_TIMEOUT_SECS`, slow requests get `408`), a **ConcurrencyLimitLayer**
(`MAX_CONCURRENT_REQUESTS`, caps in-flight work), and the body-size limit.

Liveness vs readiness are split deliberately: `/health` is dependency-free (so a
DB blip never restarts a healthy pod), while `/health/ready` checks the store
and returns `503` when it's unreachable, so a load balancer drains the instance.

### Smoke test (against a running server)

`scripts/smoke_test.sh` (optional dev tooling — `bash`+`curl`, no extra deps)
exercises the live API end to end: create → reverse-lookup redirect → metadata →
delete. See "Run it in two terminals" below.

## Memory & safety

`#![forbid(unsafe_code)]` across the crate. Configuration is injected (no
globals). Shared state is a single `Arc` (pointer-clone per request, never a data
copy). The SQLite pool is bounded (`DATABASE_MAX_CONNECTIONS`) so connection
memory is capped. Request bodies are size-limited. Shutdown is graceful so the
pool drains — no leaked connections, no `Box::leak`, no reference cycles.

## Run

```bash
cp .env.example .env        # optional; defaults work out of the box
cargo run
```

```bash
# Create — capture the returned code
curl -s -X POST http://127.0.0.1:8080/api/links \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com"}'
# -> 201 {"code":"9KGJ8rw","short_url":"http://127.0.0.1:8080/9KGJ8rw",...}

curl -i http://127.0.0.1:8080/9KGJ8rw            # 302 + Location header
curl http://127.0.0.1:8080/api/links/9KGJ8rw     # {"hits":1,...}
curl -X DELETE http://127.0.0.1:8080/api/links/9KGJ8rw   # 204
```

## Quality gates

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

## Testing

The suite runs without any external services (integration tests use the
in-memory repository; the SQLite adapter test uses a single shared `:memory:`
connection):

- **Domain unit tests** — `ShortCode`/`TargetUrl` validation, `Link` invariants.
- **Error mapping** — each `AppError` variant maps to the correct HTTP status.
- **Application use cases** — create (with/without alias), resolve increments
  hits, get, delete, duplicate-alias conflict, generated-code validity.
- **SQLite adapter** — insert / unique-violation → `Conflict` / get / increment /
  delete against an in-memory database.
- **End-to-end** (`tests/links.rs`) — full lifecycle over the real Axum app:
  create → 302 redirect (hit counted) → metadata → delete → 404; plus
  invalid-URL `400` and duplicate-alias `409`.

Run everything with `cargo test`.

## Layout

```
src/
  main.rs          Composition root (config -> repo -> app -> serve)
  lib.rs           AppState + build_app(); injects the repository
  config.rs        Env-driven Config
  error.rs         AppError -> IntoResponse (+ unit tests)
  domain/          Link, ShortCode/TargetUrl validation, LinkRepository port
  application/      LinkService use cases + ServiceError
  infrastructure/  InMemoryLinkRepository + SqliteLinkRepository (WAL-tuned)
  api/             Axum router, handlers, DTOs
migrations/        SQLite schema
tests/             health + end-to-end link lifecycle
```
