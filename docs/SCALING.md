# Scaling Analysis — URL Shortener at ~10M Users

Scope: what breaks in the **current** design (single Axum process, one local
SQLite file, bounded `sqlx` pool) if traffic grows to millions of users, with a
focus on **shared state / the shared database**. Severity is ordered.

## TL;DR

The architecture is clean and stateless *except* for one thing: all state lives
in a single SQLite file owned by one process. That single shared resource is the
ceiling. Two changes unlock almost everything else: (1) move writes off the
redirect hot path, and (2) move storage to a networked DB so the app can scale
horizontally. Until then you are limited to one box and SQLite's single-writer
throughput.

---

## 1. SQLite is a single-writer, single-node shared resource (blocker)

SQLite serializes all writes: the whole database takes one write lock at a time.
We also never set `journal_mode=WAL` or a `busy_timeout`, so the default
rollback journal makes readers and writers block each other, and contended
writes fail immediately with `SQLITE_BUSY` instead of waiting.

At 10M users this caps write throughput to roughly a few hundred–few thousand
writes/sec on one node, and — critically — **the database file is local**, so
you cannot run a second app instance against the same data (SQLite over NFS/EBS
shared mounts corrupts). That means **no horizontal scaling** as built.

Mitigations, in order of leverage:
- Move to a networked, multi-connection database (PostgreSQL) so N stateless app
  instances share one DB behind a load balancer. This is the real fix.
- If staying on SQLite short-term: enable WAL + a `busy_timeout`, keep it
  single-node, and treat it as a vertical-scale-only deployment.

## 2. A write on every redirect (write amplification)

`LinkService::resolve` does `get()` **then** `increment_hits()` — a `SELECT`
followed by an `UPDATE` (`src/application/mod.rs`). Redirects are the hottest
endpoint, so we issue one write per redirect *and* two round trips per request.
On a single-writer store this is the first thing to fall over under load.

Mitigations:
- Take counting off the hot path: increment in memory and flush in batches, or
  use Redis `INCR`, or push hit events to a queue/log and aggregate
  asynchronously. Exact, real-time counts are rarely required.
- If counts must stay in the DB, at least make resolve a single atomic
  statement (`UPDATE ... WHERE code = ? RETURNING target`) to halve round trips
  and remove the read/write race.

## 3. No read cache for hot links

Every redirect hits the database even though short-link traffic is extremely
skewed (a tiny fraction of codes get most of the hits). At scale this wastes the
scarcest resource (DB connections/locks) on data that almost never changes.

Mitigation: cache `code → target` in an in-process LRU (and/or Redis). The
mapping is immutable for a link's life, so caching is safe; invalidate on
delete. This converts the vast majority of redirects into zero-DB lookups.

## 4. Redirect is 302 (not cacheable) — origin takes every hit

302 is correct for accurate hit-counting (browsers/CDNs re-request each time),
but it means a CDN can't absorb redirect traffic — every visit reaches origin.
That is a deliberate trade-off; just know that 10M users = full redirect RPS on
your servers. If counting can be relaxed for some links, a cacheable 301 lets a
CDN serve them and removes that load entirely.

## 5. Connection pool sizing & contention

`DATABASE_MAX_CONNECTIONS` defaults to 5. Under high concurrency, requests queue
on pool checkout; with SQLite, more write connections don't help anyway (writes
serialize). On PostgreSQL you'd size the pool to the DB and front it with
PgBouncer. Either way the default of 5 is a dev value, not a 10M-user value.

## 6. Abuse controls

`POST /api/links` is unauthenticated. An opt-in per-IP rate limit and a
concurrency cap exist, but at scale you also want auth/quotas on create and a
phishing/malware domain blocklist (a host blocklist is already in place), to stop
bulk/abusive creation from bloating the DB or serving spam redirects.

## 7. Short-code space & generation (minor at 10M)

Generated codes are random 7-char base62 (~3.5×10¹² combinations). At 10M links
the per-insert collision probability is ~10⁻⁶, so the 5-retry loop is more than
enough — not a concern at this scale. It *would* matter approaching billions of
links (raise the length then). The DB unique constraint keeps it correct
regardless.

## 8. Operational gaps that bite at scale

- `/health` is liveness-only by design; `/health/ready` adds a DB-backed
  readiness check so a load balancer can drain a healthy-but-DB-broken instance.
- Structured per-request tracing exists; scrapeable metrics (e.g. Prometheus)
  are still missing — you'll want them to see latency/error spikes at 10M users.
- OS limits (file descriptors, ephemeral ports) and graceful-shutdown draining
  matter once you run many instances; the shutdown path is already in place.

---

## Suggested path to 10M users

1. **Storage:** SQLite → PostgreSQL (or a managed KV). Unblocks horizontal scaling.
2. **Hot path:** cache `code → target`; move hit-counting off the request
   (batch/async or Redis `INCR`).
3. **Horizontal:** run several stateless app instances behind a load balancer.
4. **Protect:** rate limiting + auth/quotas on create; readiness probe; metrics.
5. **Tune:** right-size the pool, add PgBouncer, set DB indexes (the `code` PK
   already covers lookups).

None of this requires changing the domain or application layers — the
`LinkRepository` port means a Postgres/Redis-backed adapter and a caching
decorator slot in without touching business logic. That is the payoff of the
layering.
