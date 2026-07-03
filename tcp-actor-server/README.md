# TCP Actor Server

A non-blocking **TCP/HTTP server** in **Rust** on the **Tokio** runtime, built
with the **actor model** and a lean dependency set. It answers a small HTTP/1.1
subset, isolates each connection in its own task, bounds concurrency, tracks live
connections through an actor, and shuts down gracefully.

> **A self-contained service.** It has no dependency on anything outside this
> folder — its own crate, lockfile, Dockerfile, and tests. It builds, runs, ships,
> and can be deleted independently of any sibling folder in this repo. No `unsafe`
> (`#![forbid(unsafe_code)]`), no `DashMap`/`scc`/`crossbeam` — shared state is
> either an owning **actor task** or a **lock-free atomic**.

## Architecture

```
                      accept loop (bounded by a Semaphore permit / connection)
                                     │  into_split()
              ┌──────────────────────┼──────────────────────┐
              ▼                      ▼                       ▼
        ┌───────────┐          ┌───────────┐          ┌───────────┐
        │ conn task │          │ conn task │   ...    │ conn task │   (one per connection,
        │ read+write│          │ read+write│          │ read+write│    owns its read/write halves)
        └─────┬─────┘          └─────┬─────┘          └─────┬─────┘
              │ register / deregister (RAII guard)          │
              ▼                                             ▼
        ┌──────────────────────┐                 ┌────────────────────┐
        │ Registry actor       │                 │ Metrics (AtomicU64) │
        │ owns HashMap<id,addr>│                 │ lock-free counters  │
        │ (no lock, messages)  │                 └────────────────────┘
        └──────────────────────┘
```

Why these choices (the same reasoning we applied elsewhere):

- **Per-connection isolation.** Each `TcpStream` is `into_split()` into a read
  half and a write half owned by *one* task. A stream is never shared between
  tasks, so there is no per-connection lock and a panicking handler kills only its
  own connection — not the server.
- **Actor for live connections.** One task owns the connection map; connection
  tasks *message* it (register / deregister / count) over a channel. No `Mutex`,
  no lock guard. Deregistration rides an RAII `Drop` guard, so a connection is
  removed even if its task panics — the live set never leaks.
- **Lock-free counters.** `connections_total` / `requests_total` are `AtomicU64`
  (`fetch_add`, `Relaxed`) — a single integer is the one place lock-free is
  unambiguously right.
- **Bounded concurrency.** The accept loop holds a `Semaphore` permit per
  connection; at `MAX_CONNECTIONS` it stops accepting (backpressure) rather than
  spawning unbounded tasks.
- **Graceful shutdown.** A `watch` flag tells the accept loop to stop and each
  connection to finish its in-flight request and close, within a grace window.

### The HTTP subset (on purpose)

This is *not* a general HTTP stack. It parses the request line and the headers it
acts on (`Content-Length`, `Connection`), consumes a `Content-Length` body to stay
framed for keep-alive, supports HTTP/1.1 keep-alive and pipelining, and rejects
what it doesn't support: chunked bodies (`501`), oversized heads (`431`),
malformed requests (`400`), oversized bodies (`413`), stalled reads (`408`). It's
enough to answer requests and be load-tested — parsing is a pure, unit-tested
function.

## Endpoints

| Method | Path | Response |
|---|---|---|
| `GET`/`HEAD` | `/` | `200` — hello text |
| `GET`/`HEAD` | `/healthz` | `200 ok` |
| `GET`/`HEAD` | `/status` | `200` — JSON: `connections_active`, `connections_total`, `requests_total` |
| `GET`/`HEAD` | other | `404` |
| other method | any | `405` |

## Run

```bash
cp .env.example .env     # optional; defaults work
cargo run                # serves on 127.0.0.1:8080
# or:
APP_BIND_ADDR=0.0.0.0:8080 MAX_CONNECTIONS=20000 cargo run --release
```

```bash
curl -i http://127.0.0.1:8080/
curl -s http://127.0.0.1:8080/status
```

## Configuration (env)

| Var | Default | Meaning |
|---|---|---|
| `APP_BIND_ADDR` | `127.0.0.1:8080` | bind address |
| `MAX_CONNECTIONS` | `10000` | concurrent-connection cap (backpressure) |
| `MAX_BODY_BYTES` | `65536` | max request body (else `413`) |
| `REQUEST_TIMEOUT_SECS` | `30` | per-read inactivity timeout |
| `SHUTDOWN_GRACE_SECS` | `10` | drain window on shutdown |
| `RUST_LOG` | `info` | log filter |

## Tests

```bash
cargo test                    # unit (http parse, metrics, registry) + integration
cargo clippy -- -D warnings
cargo fmt --check
```

Integration tests bind to port 0, drive the real server over loopback TCP
(`200`/`404`/`405`/`400`, `/status`, keep-alive pipelining, 50 concurrent
connections, graceful shutdown).

## Load testing

Performance targets (RPS, p95 latency, concurrent connections) are yours to
measure on real hardware — this repo ships the driver, not fabricated numbers:

```bash
ulimit -n 65535
MAX_CONNECTIONS=20000 cargo run --release &
./scripts/loadtest.sh http://127.0.0.1:8080/ 30s 10000 8   # needs `wrk`
```

## Quality gates

`#![forbid(unsafe_code)]`; config injected (no globals); per-connection isolation;
bounded concurrency; graceful shutdown. A standalone Cargo project — build, test,
and run entirely from this folder.
