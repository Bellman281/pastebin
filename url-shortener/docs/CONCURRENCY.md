# Concurrency & Optimization Notes

How this service shares state under load, why each piece uses the tool it uses,
and the reasoning to reuse when optimizing any similar service. This document is
self-contained — the service builds, runs, and is understood from this folder.

The one-line thesis: **there is no single "best" concurrency primitive — you
match the tool to the access pattern.** Reaching for channels (or "lock-free")
everywhere is cargo-culting; some hot paths get *slower* that way.

## The decision ladder

When two or more tasks need the same data, work down this list and stop at the
first tool that fits. Each rung is cheaper to reason about than the one below it.

1. **Don't share.** Give each task its own data. Nothing beats no coordination.
2. **Share immutable data with `Arc`.** Read-only sharing needs *no lock* — an
   `Arc` clone is an atomic ref-count bump, and reads are wait-free. This is not
   "a lock"; it's the absence of one.
3. **Short `Mutex` / `RwLock` critical section.** For a small, synchronous
   read-modify-write. `RwLock` when reads dominate (parallel readers). Hold it
   for a few lines, **never across `.await`**.
4. **Atomics for a single scalar.** A counter or flag: `AtomicU64::fetch_add`
   is genuinely lock-free, safe, no `unsafe`, no dependency.
5. **Message passing (channels / actor).** When the work is *fire-and-forget*,
   *batchable*, or genuinely *sequential*, or when you'd otherwise need to hold
   state across `.await`.
6. **Hand-rolled lock-free data structures.** Last resort. Hard to get right
   (CAS loops, ABA, memory ordering), often needs `unsafe` or a vetted crate,
   and frequently *slower* under contention than a plain `Mutex`. Only after you
   have measured a lock as the bottleneck.

> **"Lock-free is always better" is a myth.** Lock-free is a precise *progress
> guarantee*, not the absence of the word `Mutex`. An uncontended `Mutex` lock is
> a few nanoseconds; a single actor task serializes every caller onto one core,
> which is *worse* than a lock for a hot read path. Atomics (rung 4) are the one
> place lock-free is unambiguously the right call.

## What we changed, and why

### 1. In-memory repository → an actor (no lock)

`InMemoryLinkRepository` was `Mutex<HashMap<String, Link>>`. It is now an
**actor**: one background task owns the map, and callers send it commands over an
`mpsc` channel and await the answer on a per-call `oneshot` reply
(`src/infrastructure/memory.rs`).

- **Why:** it demonstrates "share memory by communicating" cleanly — no `Mutex`,
  no lock guard, no poisoning; the map is plain owned state mutated through
  `&mut` on one task, so data races are impossible *by construction*.
- **The honest tradeoff:** every operation — including reads — is now a channel
  round-trip serialized on the owning task. That is *fine for a test/dev double*
  (production is SQLite), but it is **not** what you'd want for a hot, read-heavy
  production map. For that, `RwLock` (parallel reads) or a sharded map is faster.
- The in-memory **cache** (`InMemoryCache`, another test/dev double) was converted
  to an actor the same way, purely for *consistency* — one sharing model across
  the codebase. Production caching is Redis, so there is no runtime cost either way.

### 2. Hit counter → channel-batched background writer

Every redirect used to do a synchronous `UPDATE hits = hits + 1`, putting the hot
path in contention for a DB write. Now a redirect only **enqueues** the code on a
channel (a non-blocking send) and returns; a single background task coalesces
hits and flushes them as batched `hits = hits + n` writes on an interval
(`src/hits.rs`).

- **Why it fits here:** hit counting is *fire-and-forget* (already best-effort)
  and *batchable* — the textbook case for a channel.
- **Design:** a `HitRecorder` port with two impls — `ImmediateHitRecorder`
  (one write per hit; the default, exact, used in tests) and
  `BatchingHitRecorder` (the channel batcher, wired in `main`, flushed on
  graceful shutdown so no counts are lost).
- **Repository support:** the port gained `increment_hits_by(code, n)` as the
  primitive; `increment_hits` is a default method that calls it with `1`.

### 3. Metrics → lock-free `AtomicU64`

`GET /metrics` reports redirects served. The counter is an `AtomicU64` bumped
with `fetch_add(1, Ordering::Relaxed)` and read with `load` (`src/metrics.rs`),
living in the shared `Arc<AppState>`.

- **Why `Relaxed`:** the counter guards no other memory, so we only need each
  increment to be atomic and the total eventually accurate — the cheapest
  ordering is correct. Use `Acquire`/`Release` only when an atomic *gates* access
  to other data.
- **Why this and not a `Mutex<u64>`:** a single integer is the sweet spot of
  lock-free — no lock is taken, so no thread ever blocks another.

### 4. Rate limiter → kept its `Mutex` (deliberately)

`RateLimiter` stays `Mutex<HashMap<IpAddr, Bucket>>` (`src/rate_limit.rs`).

- **Why not a channel/actor:** `check()` is a sub-microsecond synchronous
  read-modify-write on the hot path. Routing every request through one actor task
  would serialize all rate-limit checks onto one core — strictly slower.
- **Why not an atomic:** a token bucket mutates *two* fields together (tokens +
  last-refill instant), which a single atomic cannot express. The only lock-free
  direction is sharding the map (many small locks) — still locks, just finer.

### 5. `AppState` → read-only `Arc` (not a lock)

`AppState` is shared as `Arc<AppState>` and is immutable after construction. The
`Arc` (and `Arc`'s `Deref`) are *not* a mutex — sharing immutable data is
lock-free by definition. The only mutable interiors are the rate limiter's map
(rung 3) and the metric (rung 4).

## Summary table

| Concern                    | Tool                | Rung | Why |
|----------------------------|---------------------|------|-----|
| Shared config / handles    | `Arc<AppState>`     | 2    | Read-only; no lock needed |
| In-memory store (test dbl) | actor + `mpsc`      | 5    | Owns data, no lock; serialization OK for a fake |
| Rate-limit buckets         | `Mutex<HashMap>`    | 3    | Tiny synchronous critical section on the hot path |
| Redirect hit counting      | channel batcher     | 5    | Fire-and-forget + batchable |
| Redirects-served metric    | `AtomicU64`         | 4    | Single scalar, genuinely lock-free |
| Production store           | SQLite (`sqlx`)     | —    | WAL + busy_timeout for read/write concurrency |

## Dependency locks (and why they aren't the same problem)

A reasonable worry: "our crates use `Mutex` internally, so haven't we just moved
the contention?" No. Our goal was never *zero mutexes in the process* — that is
impossible and pointless. The goal was narrower: **don't hold a lock in *our*
code on a per-request hot path where a better tool fits.** A library's internal
lock is not ours to reimplement, is far more optimized than we'd hand-roll, and
is held briefly by design.

Where our direct dependencies use internal synchronization, and why it's fine:

| Crate | Internal locking | Does it matter for us? |
|-------|------------------|------------------------|
| `sqlx` (`SqlitePool`) | async semaphore for connection checkout | Bounded by `max_connections`. **The real serialization is SQLite itself: single-writer.** That's why we added WAL (concurrent readers), `busy_timeout`, and the batched counter (fewer writes). True write concurrency = Postgres. |
| `tokio` | scheduler + channel internals (often lock-free queues) | We use `tokio::sync` **channels**, not `tokio::sync::Mutex`. Highly tuned; not our hot path. |
| `tracing` | global subscriber locks when emitting events | It's I/O (logging), bounded by log level — not the data path. |
| `tower` / `tower-http` | `ConcurrencyLimitLayer` uses a semaphore | That *is its job* (bounding in-flight work); routing is otherwise lock-free. |
| `redis` (`ConnectionManager`) | internal multiplexing | Async, non-blocking, designed for concurrent use. |
| `axum`, `rand` (thread-local rng), `serde`, `url`, `thiserror` | none on our data path | — |

To inspect the transitive picture yourself:

```bash
cargo tree                    # full dependency graph
cargo tree -i parking_lot     # who pulls in the fast-mutex crate (many do; that's fine)
cargo tree -i tokio
```

The takeaway: the one lock that actually governs *our* scaling is the database's
single-writer serialization, and we already mitigate it (WAL + `busy_timeout` +
batched writes). Everything else is a library's well-tuned internal detail.

## The rule of thumb

> Reduce **contention** and time-**held**; pick the simplest tool that hits your
> latency target; then measure. A `Mutex` on a short critical section is a correct,
> honest default — not a smell. Channels win when work is fire-and-forget or
> batchable. Atomics win for single scalars. Everything else is a last resort.
