# Concurrency & Optimization Notes

How this service shares state under load, and why each piece uses the tool it
uses. This document is self-contained — the service builds, runs, and is
understood entirely from this folder.

## The decision ladder

When two or more tasks need the same data, work down this list and stop at the
first tool that fits (each rung is cheaper to reason about than the next):

1. **Don't share** — give each task its own data.
2. **Share immutable data with `Arc`** — read-only sharing needs no lock (an
   `Arc` clone is an atomic ref-count bump; reads are wait-free).
3. **Short `Mutex` / `RwLock` critical section** — a small synchronous
   read-modify-write, held for a few lines, **never across `.await`**.
4. **Atomics for a single scalar** — `AtomicU64::fetch_add`, genuinely lock-free.
5. **Message passing (channels / actor)** — when work is *fire-and-forget*,
   *batchable*, or genuinely *sequential*.
6. **Hand-rolled lock-free structures** — last resort, only after measuring a
   lock as the bottleneck.

"Lock-free everywhere" is a myth: lock-free is a precise progress guarantee, not
the absence of `Mutex`, and a single actor task serializes callers onto one core.
Atomics (rung 4) are the one place lock-free is unambiguously right. Match the
tool to the access pattern.

## What this service does

| Concern                    | Tool                | Why |
|----------------------------|---------------------|-----|
| Shared config / handles    | `Arc<AppState>`     | Read-only; no lock needed |
| In-memory store (test dbl) | actor + `mpsc`      | Owns the map, no lock; serialization is fine for a fake (`src/infrastructure/memory.rs`) |
| Rate-limit buckets         | `Mutex<HashMap>`    | Sub-microsecond synchronous critical section on the hot path (`src/rate_limit.rs`) |
| Paste view counting        | channel batcher     | Fire-and-forget + batchable (`src/views.rs`) |
| Pastes-served metric       | `AtomicU64`         | Single scalar, genuinely lock-free (`src/metrics.rs`) |
| Production store           | SQLite (`sqlx`)     | WAL + busy_timeout for read/write concurrency |

## Notes specific to the pastebin

- **View counting is batched, burn-after-read is atomic (never batched).** A
  non-one-shot fetch only *enqueues* the paste id on a channel; a background task
  coalesces and flushes `views = views + n`. A `one_shot` paste is instead claimed
  with an **atomic `take`** (remove-and-return in one operation — `DELETE …
  RETURNING` in SQLite, a single actor message in memory), so under concurrent
  fetches exactly **one** reader serves it and the losers get `NotFound`. This must
  never be deferred, so it stays on the request path.
- **Rate-limiter memory is bounded.** A background sweeper evicts buckets idle
  beyond a threshold, so the per-IP map can't grow without limit under a flood of
  distinct source IPs. The limiter still uses a `Mutex` (correct for its tiny
  hot-path critical section).
- **Rate-limit key is proxy-aware (opt-in).** With `trust_proxy` set, the client
  IP comes from `X-Forwarded-For` / `X-Real-IP`; off by default (socket peer IP)
  so the header can't be spoofed to bypass the limit.
- **Repository port.** `increment_views_by(id, n)` is the primitive every backend
  implements; `increment_views` is a default method calling it with `1`.
- **Recorder port.** `ViewRecorder` has `ImmediateViewRecorder` (one write per
  view; the default, exact, used in tests) and `BatchingViewRecorder` (the channel
  batcher, wired in `main`, flushed on graceful shutdown so no counts are lost).
- **Metrics.** `GET /metrics` returns `{"pastes_served": N}`, an `AtomicU64`
  bumped on each successfully served fetch (`get_paste` and `raw_paste`).
- **What we did *not* change.** The rate limiter keeps its `Mutex` (correct for a
  tiny hot-path critical section), and `AppState` is shared read-only via `Arc`
  (lock-free by definition).
