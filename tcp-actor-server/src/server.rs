//! The TCP server: accept loop, per-connection tasks, routing, graceful shutdown.
//!
//! Concurrency model (see the other services' `docs/CONCURRENCY.md` for the same
//! reasoning applied elsewhere):
//! - **Per-connection isolation.** Each accepted `TcpStream` is `into_split()` into
//!   a read half and a write half owned by *one* spawned task. A stream is never
//!   shared between tasks, so there is no lock on the connection and a panicking
//!   handler takes down only its own connection.
//! - **Bounded concurrency.** The accept loop holds a `Semaphore` permit per
//!   connection; at `max_connections` it stops accepting (backpressure) instead of
//!   spawning unbounded tasks.
//! - **Slow-loris resistance.** Once a request begins, the *entire* request (head
//!   and body) must arrive within one `request_timeout` deadline — a per-read
//!   timeout can be reset forever by trickling one byte at a time; a whole-request
//!   deadline cannot.
//! - **Shared state without locks.** Live connections are owned by the registry
//!   actor; counters are lock-free atomics; config is read-only behind `Arc`.
//! - **Graceful shutdown.** A `watch` flag tells the accept loop to stop and each
//!   connection to finish its in-flight request and close; the server then *waits*
//!   for connections to actually drain (by re-acquiring every semaphore permit),
//!   bounded by the grace period.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::config::Config;
use crate::http::{build_response, parse_head, ParseError, RequestHead};
use crate::metrics::Metrics;
use crate::registry::Registry;

/// Read-only shared state injected into every connection task via `Arc`.
struct AppState {
    config: Config,
    metrics: Metrics,
    registry: Registry,
}

/// A bound TCP server, ready to run.
pub struct Server {
    listener: TcpListener,
    state: Arc<AppState>,
}

impl Server {
    /// Bind the listener and build shared state (spawns the registry actor).
    pub async fn bind(config: Config) -> io::Result<Self> {
        let listener = TcpListener::bind(config.bind_addr).await?;
        let state = Arc::new(AppState {
            config,
            metrics: Metrics::default(),
            registry: Registry::spawn(),
        });
        Ok(Self { listener, state })
    }

    /// The address actually bound (useful when binding to port 0 in tests).
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serve until `shutdown` resolves, then wait for in-flight connections to
    /// drain (bounded by the grace period).
    pub async fn run(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> io::Result<()> {
        // A watch flag broadcasts "stop" to the accept loop and every connection.
        let (flag_tx, flag_rx) = watch::channel(false);
        tokio::spawn(async move {
            shutdown.await;
            let _ = flag_tx.send(true);
        });

        let max_conns = self.state.config.max_connections;
        let limit = Arc::new(Semaphore::new(max_conns));
        let next_id = AtomicU64::new(1);
        let mut brk = flag_rx.clone();

        loop {
            if *brk.borrow() {
                break;
            }
            // Acquire a permit first (bounds concurrency); interruptible by shutdown.
            let permit = tokio::select! {
                biased;
                _ = brk.changed() => break,
                p = limit.clone().acquire_owned() => match p {
                    Ok(p) => p,
                    Err(_) => break, // semaphore closed — never in practice
                },
            };

            let (stream, addr) = tokio::select! {
                biased;
                _ = brk.changed() => { drop(permit); break; }
                res = self.listener.accept() => match res {
                    Ok(pair) => pair,
                    Err(e) => {
                        warn!(error = %e, "accept failed");
                        drop(permit);
                        // Back off briefly so a persistent error (e.g. EMFILE)
                        // doesn't spin the loop hot.
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        continue;
                    }
                },
            };

            let id = next_id.fetch_add(1, Ordering::Relaxed);
            let state = self.state.clone();
            let conn_shutdown = flag_rx.clone();
            tokio::spawn(async move {
                handle_connection(id, stream, addr, state, conn_shutdown).await;
                drop(permit); // release the slot when the connection ends
            });
        }

        // Wait for in-flight connections to actually finish. Each live connection
        // holds one permit, so acquiring *all* of them succeeds only once every
        // connection task has released its permit (i.e. ended). Bounded by grace.
        let max = u32::try_from(max_conns).unwrap_or(u32::MAX);
        match tokio::time::timeout(
            self.state.config.shutdown_grace,
            Arc::clone(&limit).acquire_many_owned(max),
        )
        .await
        {
            Ok(_) => {} // all connections drained (or the semaphore closed)
            Err(_) => warn!("shutdown grace elapsed with connections still draining"),
        }
        Ok(())
    }
}

/// Handle one connection to completion: read requests, route, respond, honoring
/// keep-alive, the per-request deadline, and the shutdown flag.
async fn handle_connection(
    id: u64,
    stream: TcpStream,
    addr: SocketAddr,
    state: Arc<AppState>,
    mut shutdown: watch::Receiver<bool>,
) {
    state.metrics.on_connect();
    let _guard = state.registry.guard(id, addr); // Drop deregisters (even on panic)
    let _ = stream.set_nodelay(true); // best-effort: lower latency
    let (mut rd, mut wr) = stream.into_split();
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let request_timeout = state.config.request_timeout;

    'conn: loop {
        // Deadline for the *current* request, armed when its first bytes arrive.
        // Bounding the whole request (not each read) is what defeats slow-loris.
        let mut deadline: Option<Instant> = None;

        // Read until a full request head is buffered.
        let (head, head_len) = loop {
            match parse_head(&buf) {
                Ok(parsed) => break parsed,
                Err(ParseError::Incomplete) => {}
                Err(ParseError::HeadTooLarge) => {
                    respond(
                        &mut wr,
                        431,
                        "Request Header Fields Too Large",
                        "header too large\n",
                    )
                    .await;
                    break 'conn;
                }
                Err(ParseError::Unsupported) => {
                    respond(&mut wr, 501, "Not Implemented", "unsupported framing\n").await;
                    break 'conn;
                }
                Err(ParseError::Malformed) => {
                    respond(&mut wr, 400, "Bad Request", "bad request\n").await;
                    break 'conn;
                }
            }

            if buf.is_empty() {
                // Idle between requests: wait for the next one to begin, but close
                // promptly on shutdown or after an idle `request_timeout`.
                if *shutdown.borrow() {
                    break 'conn;
                }
                match read_idle(&mut rd, &mut buf, &mut shutdown, request_timeout).await {
                    ReadOutcome::Bytes => {}
                    _ => break 'conn, // idle timeout / EOF / shutdown / error → close
                }
            } else {
                // A request is in progress: the whole head must arrive by the deadline.
                let dl = *deadline.get_or_insert_with(|| Instant::now() + request_timeout);
                match read_until(&mut rd, &mut buf, dl).await {
                    ReadOutcome::Bytes => {}
                    ReadOutcome::Timeout => {
                        respond(&mut wr, 408, "Request Timeout", "request timeout\n").await;
                        break 'conn;
                    }
                    ReadOutcome::Eof => {
                        respond(&mut wr, 400, "Bad Request", "incomplete request\n").await;
                        break 'conn;
                    }
                    ReadOutcome::Shutdown | ReadOutcome::Err => break 'conn,
                }
            }
        };

        // Bound and buffer the body, under the same per-request deadline.
        if head.content_length > state.config.max_body_bytes {
            respond(&mut wr, 413, "Payload Too Large", "payload too large\n").await;
            break 'conn;
        }
        let total = head_len + head.content_length;
        let dl = deadline.unwrap_or_else(|| Instant::now() + request_timeout);
        while buf.len() < total {
            match read_until(&mut rd, &mut buf, dl).await {
                ReadOutcome::Bytes => {}
                ReadOutcome::Timeout => {
                    respond(&mut wr, 408, "Request Timeout", "request timeout\n").await;
                    break 'conn;
                }
                _ => break 'conn, // EOF / error mid-body → abort
            }
        }

        // Route and respond.
        state.metrics.on_request();
        let keep_alive = head.keep_alive && !*shutdown.borrow();
        let is_head = head.method == "HEAD";
        let (status, reason, body) = route(&head, &state).await;
        let resp = build_response(status, reason, &body, keep_alive, !is_head);
        if wr.write_all(&resp).await.is_err() {
            break 'conn;
        }
        debug!(id, %addr, method = %head.method, path = %head.path, status, "served");

        buf.drain(..total); // keep any pipelined leftover
        if !keep_alive {
            break 'conn;
        }
    }
}

/// Write a small error/response and ignore the result (we're about to close).
async fn respond(wr: &mut tokio::net::tcp::OwnedWriteHalf, status: u16, reason: &str, body: &str) {
    let _ = wr
        .write_all(&build_response(status, reason, body, false, true))
        .await;
}

/// Route a request to `(status, reason, body)`.
async fn route(head: &RequestHead, state: &AppState) -> (u16, &'static str, String) {
    match (head.method.as_str(), head.path.as_str()) {
        ("GET" | "HEAD", "/") => (200, "OK", "Hello from tcp-actor-server\n".to_owned()),
        ("GET" | "HEAD", "/healthz") => (200, "OK", "ok\n".to_owned()),
        ("GET" | "HEAD", "/status") => {
            let active = state.registry.active_count().await;
            let body = format!(
                "{{\"connections_active\":{},\"connections_total\":{},\"requests_total\":{}}}\n",
                active,
                state.metrics.connections_total(),
                state.metrics.requests_total(),
            );
            (200, "OK", body)
        }
        ("GET" | "HEAD", _) => (404, "Not Found", "not found\n".to_owned()),
        _ => (405, "Method Not Allowed", "method not allowed\n".to_owned()),
    }
}

/// Outcome of a single read.
enum ReadOutcome {
    Bytes,
    Eof,
    Timeout,
    Shutdown,
    Err,
}

/// One read while *idle* between requests: bounded by `idle` and interrupted by
/// shutdown, so keep-alive connections close promptly.
async fn read_idle(
    rd: &mut tokio::net::tcp::OwnedReadHalf,
    buf: &mut Vec<u8>,
    shutdown: &mut watch::Receiver<bool>,
    idle: std::time::Duration,
) -> ReadOutcome {
    let mut tmp = [0u8; 8192];
    let result = tokio::select! {
        biased;
        _ = shutdown.changed() => return ReadOutcome::Shutdown,
        r = tokio::time::timeout(idle, rd.read(&mut tmp)) => r,
    };
    classify(result, buf, &tmp)
}

/// One read while a request is *in progress*: bounded by the absolute
/// per-request `deadline` (not reset per read), which is what stops slow-loris.
async fn read_until(
    rd: &mut tokio::net::tcp::OwnedReadHalf,
    buf: &mut Vec<u8>,
    deadline: Instant,
) -> ReadOutcome {
    let mut tmp = [0u8; 8192];
    let result = tokio::time::timeout_at(deadline, rd.read(&mut tmp)).await;
    classify(result, buf, &tmp)
}

/// Turn a `timeout` result into a [`ReadOutcome`], appending any bytes read.
fn classify(
    result: Result<io::Result<usize>, tokio::time::error::Elapsed>,
    buf: &mut Vec<u8>,
    tmp: &[u8],
) -> ReadOutcome {
    match result {
        Ok(Ok(0)) => ReadOutcome::Eof,
        Ok(Ok(n)) => {
            buf.extend_from_slice(&tmp[..n]);
            ReadOutcome::Bytes
        }
        Ok(Err(_)) => ReadOutcome::Err,
        Err(_elapsed) => ReadOutcome::Timeout,
    }
}
