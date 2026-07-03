//! Connection registry — an **actor** that owns the set of live connections.
//!
//! One background task owns a `HashMap<id, peer addr>`; connection tasks never
//! share it, they *message* it over a channel (register / deregister / query).
//! There is no `Mutex` and no lock guard — the map is plain owned state mutated
//! on one task, so it can't be corrupted by concurrent access.
//!
//! Deregistration is tied to a [`ConnGuard`] whose `Drop` sends the message, so
//! a connection is removed even if its task panics or exits early — the set
//! never leaks a dead connection. (`Drop` can run the send because the channel
//! is unbounded, so enqueuing never blocks or fails except on shutdown.)

use std::collections::HashMap;
use std::net::SocketAddr;

use tokio::sync::{mpsc, oneshot};

/// Messages the owning task understands.
enum Cmd {
    Register { id: u64, addr: SocketAddr },
    Deregister { id: u64 },
    ActiveCount { reply: oneshot::Sender<usize> },
}

/// Handle to the connection registry. Cloning shares the same actor.
#[derive(Clone)]
pub struct Registry {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl Registry {
    /// Spawn the owning task and return a handle. Must be called from within a
    /// Tokio runtime.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(rx));
        Self { tx }
    }

    /// Register a connection and return a guard that deregisters it on drop
    /// (including on panic/unwind), so the live set stays accurate.
    pub fn guard(&self, id: u64, addr: SocketAddr) -> ConnGuard {
        let _ = self.tx.send(Cmd::Register { id, addr });
        ConnGuard {
            tx: self.tx.clone(),
            id,
        }
    }

    /// Current number of live connections. Returns `0` if the actor has stopped.
    pub async fn active_count(&self) -> usize {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Cmd::ActiveCount { reply }).is_err() {
            return 0;
        }
        rx.await.unwrap_or(0)
    }
}

/// Deregisters its connection from the registry when dropped.
pub struct ConnGuard {
    tx: mpsc::UnboundedSender<Cmd>,
    id: u64,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Deregister { id: self.id });
    }
}

/// The actor loop: sole owner of the connection map.
async fn run(mut rx: mpsc::UnboundedReceiver<Cmd>) {
    let mut conns: HashMap<u64, SocketAddr> = HashMap::new();
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Register { id, addr } => {
                conns.insert(id, addr);
            }
            Cmd::Deregister { id } => {
                conns.remove(&id);
            }
            Cmd::ActiveCount { reply } => {
                let _ = reply.send(conns.len());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> SocketAddr {
        "127.0.0.1:1234".parse().unwrap()
    }

    #[tokio::test]
    async fn guard_registers_and_drop_deregisters() {
        let reg = Registry::spawn();
        assert_eq!(reg.active_count().await, 0);

        let g1 = reg.guard(1, addr());
        let g2 = reg.guard(2, addr());
        assert_eq!(reg.active_count().await, 2);

        drop(g1);
        assert_eq!(reg.active_count().await, 1);
        drop(g2);
        assert_eq!(reg.active_count().await, 0);
    }

    #[tokio::test]
    async fn deregister_runs_even_if_the_task_panics() {
        let reg = Registry::spawn();
        let reg2 = reg.clone();
        // A task that registers then panics — the guard's Drop must still fire.
        let _ = tokio::spawn(async move {
            let _g = reg2.guard(7, addr());
            panic!("boom");
        })
        .await;
        assert_eq!(reg.active_count().await, 0);
    }
}
