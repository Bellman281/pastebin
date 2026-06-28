# Design Rationale — URL Shortener

Why the system is built the way it is. Pairs with
[`ARCHITECTURE.md`](./ARCHITECTURE.md) (conventions) and [`SCALING.md`](./SCALING.md)
(path to ~10M users). This document records the *reasoning and the positions
taken*, not just the structure.

## 1. Shared state: why there is (almost) none

In production the service has **no shared mutable state**. The only things shared
across concurrent request tasks are an `Arc<Config>` (immutable) and the `sqlx`
connection pool (a managed resource that handles its own concurrency). Every
piece of mutable state lives in the database. Handlers derive their output from
the request plus injected, read-only dependencies.

`Arc<dyn LinkRepository>` is **shared ownership of an immutable handle**, not
shared mutable state — no lock sits on the production request path. This is the
distinction that matters: shared *ownership* is benign; shared *mutable state*
is the thing that causes contention, lost updates, and single-node lock-in.

What we would pay if we introduced in-process shared state (e.g. an
`Arc<RwLock<LruCache>>` for hot links, or an `AtomicU64` for code generation):

- **Loss of horizontal scalability.** Two instances would hold divergent
  caches/counters; an in-process ID counter would mint colliding codes across
  instances unless coordinated. Share-nothing is the *precondition* for running
  many interchangeable instances behind a load balancer.
- **Contention and fragility.** A global lock serializes the hot path; the state
  is lost on restart; concurrency reasoning gets harder.

**Position:** keep the app share-nothing. When shared data is genuinely needed
(cache, counters), push it to a system built to be shared (Redis / the DB), so
instances stay stateless. The `LinkRepository` port lets such a store slot in as
an adapter without touching domain or application code.

The only in-process shared mutable state in the repo is
`InMemoryLinkRepository` (`Mutex<HashMap>`) — the test/dev double, never used in
production.

## 2. CAP theorem, applied per dataset

CAP applies only to a *distributed* deployment, and states: during a network
**partition** you must choose **Consistency** or **Availability** (partition
tolerance is not optional — networks fail). The leverage comes from applying it
**per data type**, because our data has different needs:

| Data | Consistency need | CAP lean | Why |
|---|---|---|---|
| `code → target` (create) | Strong | **CP** | The unique-code guarantee must hold; better to reject a create under partition than mint a duplicate. The DB's uniqueness is the arbiter. |
| Redirect reads | Trivial | **AP** | A mapping is **immutable after creation**, so it can be cached/replicated freely — immutable data can't go stale (except the narrow create-then-read and post-delete windows). High availability for reads is nearly free. |
| Hit counts | Relaxed | **AP / eventual** | Approximate, batched counts are fine for analytics. This is why channel-batching or Redis `INCR` is acceptable here but would be wrong for billing. |

Single-node SQLite today sidesteps CAP (no partitions) but at the cost of no
availability under failure and no scale-out. The distributed story is: **decide
C-vs-A per dataset, then choose stores accordingly** — a consistent primary for
writes, cached/replicated reads, eventually-consistent counters.

## 3. Architecture alternatives considered

| Option | Verdict | Reasoning |
|---|---|---|
| **Hexagonal stateless monolith** (chosen) | ✅ Baseline | Keeps options open; the `LinkRepository` port lets storage evolve (Postgres/Redis/DynamoDB/edge-KV) without rewriting logic. Cheap boilerplate, full unit-testability. |
| Layered / MVC monolith (no port seam) | ❌ | Storage leaks into business logic; can't unit-test without a DB. The seam is cheap and we explicitly plan to swap SQLite → Postgres/Redis. |
| Actor model (channels owning state) | ◐ As an optimization | Lock-free in-process concurrency — a good fit for the hit-counter — but state is local/non-durable, so it is an optimization, not the source of truth. |
| CQRS / read replicas | ◐ Partially | Immutable-read caching already gives most of the benefit; full CQRS machinery is overkill for a shortener. |
| Serverless + managed KV (Lambda + DynamoDB) | ◐ Strong at scale | Textbook fit: `code` as partition key, single-digit-ms lookups, auto-scale, pay-per-use. Cost: vendor lock-in, cold starts. |
| Edge + KV (Cloudflare Workers + KV/D1) | ◐ Best for reads | Pushes redirects to the edge for global low latency, eventual consistency. Ideal for read-heavy redirects. |
| Microservices | ❌ | A shortener is a single bounded context; the only scaling axis is read-vs-write, handled by caching/CQRS within one service. |

**Position taken.** For the stated goals (clean, SOLID, Rust, evolvable) the
hexagonal stateless monolith is correct *because it does not commit us*. If 10M
global users were a hard requirement today, redirects would go on **edge-KV**
with this Rust service as the origin/admin write API — but that is not built
before traffic justifies it. Microservices are explicitly rejected for this
domain.

## 4. Short-code generation & the same-millisecond question

`generate_code` (`src/application/mod.rs`): `rand::thread_rng()` draws 7
characters from a base62 alphabet and collects them into a `String`.

- **No timestamp is part of the code.** `created_at` is stored as separate
  metadata, never mixed into the code. Putting time in the code would leak
  creation time, shrink the random space, and make codes partially predictable
  and enumerable.
- **`thread_rng` is a per-thread CSPRNG** (ChaCha-based, OS-seeded). Each draw is
  independent; being per-thread means no shared RNG and no lock. As a bonus,
  cryptographic strength makes codes unguessable (enumeration-resistant).

**Several requests in the same millisecond?** Irrelevant here, because the code
does not depend on time — each request pulls independent random draws, so there
is no time-correlated collision. The only collision source is a birthday clash
in the random space, resolved authoritatively by the DB: the `code` primary key
lets one insert win, the loser receives a unique violation, and `create` retries
with a fresh code (bounded to 5 attempts). At 62⁷ ≈ 3.5×10¹² combinations the
clash rate is ~10⁻⁶ even at 10M links.

**Contrast with timestamp-based IDs (e.g. Snowflake):** those specifically
struggle with same-millisecond requests and solve it with a *per-node sequence
counter* — i.e. shared mutable state. Going random + letting the DB arbitrate
avoids that entire class of problem and keeps the service share-nothing. The one
caveat with random codes is RNG quality, which `thread_rng` satisfies.

## 5. Duplicate policy

- **Duplicate codes** are impossible by construction: `code` is the table
  `PRIMARY KEY`, so a clashing insert is a unique violation mapped to `Conflict`.
- **Duplicate target URLs are intentionally allowed** — each create is its own
  link with its own code, optional alias, and independent hit count (mainstream
  shortener behaviour). Enforcing one-code-per-URL would mean a unique index on
  `target` plus a pre-insert lookup, trading away custom aliases and per-link
  analytics. Not worth it by default.

## 6. Read/write concurrency

- **Correctness:** hits use `UPDATE … SET hits = hits + 1`, a single atomic
  read-modify-write, so concurrent redirects never lose counts. Doing the
  arithmetic in application code would create a lost-update race.
- **Throughput:** SQLite allows one writer at a time. WAL (enabled) gives
  concurrent readers + one writer; `busy_timeout` makes writers wait instead of
  erroring. Heavy concurrency ultimately wants PostgreSQL (MVCC: readers never
  block writers) and/or moving hit-counting off the request path.
