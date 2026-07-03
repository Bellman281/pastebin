//! End-to-end tests driven through the real server over loopback TCP.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

use tcp_actor_server::{Config, Server};

fn test_config() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        max_connections: 1024,
        max_body_bytes: 64 * 1024,
        request_timeout: Duration::from_secs(5),
        shutdown_grace: Duration::from_millis(0),
    }
}

struct Running {
    addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<()>,
}

async fn start() -> Running {
    let server = Server::bind(test_config()).await.unwrap();
    let addr = server.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = server
            .run(async move {
                let _ = rx.await;
            })
            .await;
    });
    Running {
        addr,
        shutdown: Some(tx),
        handle,
    }
}

/// Send a raw request and read the whole response (the server sends
/// `connection: close` and closes, so `read_to_end` returns the full reply).
async fn round_trip(addr: SocketAddr, raw: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(raw.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test]
async fn get_root_returns_200() {
    let srv = start().await;
    let resp = round_trip(
        srv.addr,
        "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"), "{resp}");
    assert!(resp.contains("Hello from tcp-actor-server"));
}

#[tokio::test]
async fn unknown_path_is_404() {
    let srv = start().await;
    let resp = round_trip(srv.addr, "GET /nope HTTP/1.1\r\nConnection: close\r\n\r\n").await;
    assert!(resp.starts_with("HTTP/1.1 404 Not Found\r\n"), "{resp}");
}

#[tokio::test]
async fn unsupported_method_is_405() {
    let srv = start().await;
    let resp = round_trip(srv.addr, "DELETE / HTTP/1.1\r\nConnection: close\r\n\r\n").await;
    assert!(
        resp.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"),
        "{resp}"
    );
}

#[tokio::test]
async fn malformed_request_is_400() {
    let srv = start().await;
    let resp = round_trip(srv.addr, "GARBAGE\r\n\r\n").await;
    assert!(resp.starts_with("HTTP/1.1 400 Bad Request\r\n"), "{resp}");
}

#[tokio::test]
async fn status_endpoint_reports_counters() {
    let srv = start().await;
    let resp = round_trip(
        srv.addr,
        "GET /status HTTP/1.1\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(resp.contains("\"connections_total\":"), "{resp}");
    assert!(resp.contains("\"requests_total\":"), "{resp}");
}

#[tokio::test]
async fn keep_alive_serves_two_pipelined_requests() {
    let srv = start().await;
    // First request keep-alive, second closes — both handled on one connection.
    let raw = "GET / HTTP/1.1\r\nHost: x\r\n\r\nGET /healthz HTTP/1.1\r\nConnection: close\r\n\r\n";
    let resp = round_trip(srv.addr, raw).await;
    assert_eq!(resp.matches("HTTP/1.1 200 OK").count(), 2, "{resp}");
    assert!(resp.contains("ok\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handles_many_concurrent_connections() {
    let srv = start().await;
    let mut handles = Vec::new();
    for _ in 0..50 {
        let addr = srv.addr;
        handles.push(tokio::spawn(async move {
            round_trip(addr, "GET / HTTP/1.1\r\nConnection: close\r\n\r\n").await
        }));
    }
    for h in handles {
        let resp = h.await.unwrap();
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "{resp}");
    }
}

#[tokio::test]
async fn graceful_shutdown_completes() {
    let mut srv = start().await;
    srv.shutdown.take().unwrap().send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), srv.handle)
        .await
        .expect("server did not shut down in time")
        .expect("server task panicked");
}
