# Concurrency & Optimization Notes

This service applies the same concurrency patterns as the sibling url-shortener.
For the full rationale — the decision ladder and *why* each tool is chosen — see
that service's [`docs/CONCURRENCY.md`](../../url-shortener/docs/CONCURRENCY.md).
Here is how the patterns land in the pastebin.

## The decision ladder (short form)

Don't share → share immutable data with `Arc` (no lock) → short `Mutex`/`RwLock`
critical section → atomics for a single scalar → message passing (channels/actor)
for fire-and-forget / batchable / sequential work → hand-rolled lock-free only
after measuring. "Lock-free everywhere" is a myth; match the tool to the access
pattern.

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

- **View counting is batched, burn-after-read is not.** A non-one-shot fetch only
  *enqueues* the paste id on a channel; a background task coalesces and flushes
  `views = views + n`. A `one_shot` paste, by contrast, is **deleted synchronously**
  on first read — that must never be deferred, so it stays on the request path and
  is never routed through the batcher.
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
