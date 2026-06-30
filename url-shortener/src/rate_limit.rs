//! A small in-process, per-IP token-bucket rate limiter.
//!
//! Each client IP gets a bucket of `burst` tokens that refills at `rps` tokens
//! per second; a request consumes one token, and is rejected when the bucket is
//! empty. `rps == 0` disables limiting entirely.
//!
//! Scope: this is a **per-instance** limiter — each replica counts
//! independently. A globally-consistent limit across replicas needs a shared
//! store (e.g. Redis). The bucket map grows with distinct IPs; for very large
//! deployments add periodic eviction.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// Thread-safe per-IP token-bucket limiter.
#[derive(Debug)]
pub struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

impl RateLimiter {
    /// Build a limiter. `rps == 0` disables limiting. When `burst == 0` but
    /// `rps > 0`, the burst defaults to `rps`.
    pub fn new(rps: u32, burst: u32) -> Self {
        let capacity = if burst == 0 { rps } else { burst };
        Self {
            capacity: capacity as f64,
            refill_per_sec: rps as f64,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Returns true if the request is allowed (and consumes a token).
    pub fn check(&self, ip: IpAddr) -> bool {
        if self.refill_per_sec <= 0.0 {
            return true; // disabled
        }
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let bucket = buckets.entry(ip).or_insert(Bucket {
            tokens: self.capacity,
            last: now,
        });
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn rps_zero_disables_limiting() {
        let rl = RateLimiter::new(0, 0);
        let a = ip("1.2.3.4");
        for _ in 0..1000 {
            assert!(rl.check(a));
        }
    }

    #[test]
    fn burst_is_consumed_then_blocked() {
        let rl = RateLimiter::new(1, 1); // 1 token, refills 1/sec
        let a = ip("1.2.3.4");
        assert!(rl.check(a)); // consumes the only token
        assert!(!rl.check(a)); // immediately after: ~0 refill -> blocked
    }

    #[test]
    fn buckets_are_independent_per_ip() {
        let rl = RateLimiter::new(1, 1);
        let a = ip("1.1.1.1");
        let b = ip("2.2.2.2");
        assert!(rl.check(a));
        assert!(!rl.check(a));
        assert!(rl.check(b)); // b has its own full bucket
    }
}
