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
//! - **Shared state without locks.** Live connections are owned by the registry
//!   actor; counters are lock-free atomics; config is read-only behind `Arc`.
//! - **Graceful shutdown.** A `watch` flag tells the accept loop to stop and each
//!   connection to finish its in-flight request and close.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
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

    /// Serve until `shutdown` resolves, then let in-flight connections drain.
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

        let limit = Arc::new(Semaphore::new(self.state.config.max_connections));
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

        // Let in-flight connections observe the flag and finish.
        tokio::time::sleep(self.state.config.shutdown_grace).await;
        Ok(())
    }
}

/// Handle one connection to completion: read requests, route, respond, honoring
/// keep-alive, timeouts, and the shutdown flag.
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
    let timeout = state.config.request_timeout;

    'conn: loop {
        // Read until a full request head is buffered.
        let (head, head_len) = loop {
            match parse_head(&buf) {
                Ok(parsed) => break parsed,
                Err(ParseError::Incomplete) => {}
                Err(ParseError::HeadTooLarge) => {
                    let _ = wr
                        .write_all(&build_response(
                            431,
                            "Request Header Fields Too Large",
                            "header too large\n",
                            false,
                            true,
                        ))
                        .await;
                    break 'conn;
                }
                Err(ParseError::Unsupported) => {
                    let _ = wr
                        .write_all(&build_response(
                            501,
                            "Not Implemented",
                            "unsupported framing\n",
                            false,
                            true,
                        ))
                        .await;
                    break 'conn;
                }
                Err(ParseError::Malformed) => {
                    let _ = wr
                        .write_all(&build_response(
                            400,
                            "Bad Request",
                            "bad request\n",
                            false,
                            true,
                        ))
                        .await;
                    break 'conn;
                }
            }

            let waiting_new = buf.is_empty();
            if waiting_new && *shutdown.borrow() {
                break 'conn; // shutting down and idle → close cleanly
            }
            match read_some(&mut rd, &mut buf, &mut shutdown, timeout, waiting_new).await {
                ReadOutcome::Bytes => {}
                ReadOutcome::Eof => {
                    if !buf.is_empty() {
                        let _ = wr
                            .write_all(&build_response(
                                400,
                                "Bad Request",
                                "incomplete request\n",
                                false,
                                true,
                            ))
                            .await;
                    }
                    break 'conn;
                }
                ReadOutcome::Timeout => {
                    if !buf.is_empty() {
                        let _ = wr
                            .write_all(&build_response(
                                408,
                                "Request Timeout",
                                "request timeout\n",
                                false,
                                true,
                            ))
                            .await;
                    }
                    break 'conn;
                }
                ReadOutcome::Shutdown | ReadOutcome::Err => break 'conn,
            }
        };

        // Bound and buffer the body.
        if head.content_length > state.config.max_body_bytes {
            let _ = wr
                .write_all(&build_response(
                    413,
                    "Payload Too Large",
                    "payload too large\n",
                    false,
                    true,
                ))
                .await;
            break 'conn;
        }
        let total = head_len + head.content_length;
        while buf.len() < total {
            match read_some(&mut rd, &mut buf, &mut shutdown, timeout, false).await {
                ReadOutcome::Bytes => {}
                _ => break 'conn, // EOF / timeout / error mid-body → abort
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

/// Outcome of a single timed read.
enum ReadOutcome {
    Bytes,
    Eof,
    Timeout,
    Shutdown,
    Err,
}

/// Do one timed read into `buf`. When `waiting_new` (between requests), a
/// shutdown also interrupts so idle keep-alive connections close promptly.
async fn read_some(
    rd: &mut tokio::net::tcp::OwnedReadHalf,
    buf: &mut Vec<u8>,
    shutdown: &mut watch::Receiver<bool>,
    timeout: std::time::Duration,
    waiting_new: bool,
) -> ReadOutcome {
    let mut tmp = [0u8; 8192];
    let read = tokio::time::timeout(timeout, rd.read(&mut tmp));

    let result = if waiting_new {
        tokio::select! {
            biased;
            _ = shutdown.changed() => return ReadOutcome::Shutdown,
            r = read => r,
        }
    } else {
        read.await
    };

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
