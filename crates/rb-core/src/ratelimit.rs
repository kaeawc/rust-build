//! Token-bucket rate limiter driven by a [`Clock`](crate::clock::Clock).

use crate::clock::{SharedClock, lock};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Bucket {
    rate_per_sec: f64,
    burst: f64,
    clock: SharedClock,
    state: Mutex<(f64, Instant)>,
}

impl Bucket {
    /// Refills `rate_per_sec` tokens per second up to `burst`; starts full.
    pub fn new(rate_per_sec: f64, burst: u32, clock: SharedClock) -> Self {
        let now = clock.now();
        Bucket { rate_per_sec, burst: f64::from(burst), clock, state: Mutex::new((f64::from(burst), now)) }
    }

    fn refill(&self, s: &mut (f64, Instant)) {
        let now = self.clock.now();
        let elapsed = now.saturating_duration_since(s.1).as_secs_f64();
        s.0 = (s.0 + elapsed * self.rate_per_sec).min(self.burst);
        s.1 = now;
    }

    pub fn allow(&self) -> bool {
        self.allow_n(1)
    }

    /// Takes `n` tokens if available.
    pub fn allow_n(&self, n: u32) -> bool {
        let mut s = lock(&self.state);
        self.refill(&mut s);
        if s.0 >= f64::from(n) {
            s.0 -= f64::from(n);
            true
        } else {
            false
        }
    }

    /// How long until `n` tokens are available (zero if now). Callers sleep this long and retry.
    pub fn wait_time(&self, n: u32) -> Duration {
        let mut s = lock(&self.state);
        self.refill(&mut s);
        let missing = f64::from(n) - s.0;
        if missing <= 0.0 || self.rate_per_sec <= 0.0 { Duration::ZERO } else { Duration::from_secs_f64(missing / self.rate_per_sec) }
    }

    pub fn available(&self) -> f64 {
        let mut s = lock(&self.state);
        self.refill(&mut s);
        s.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Fake;
    use std::sync::Arc;

    #[test]
    fn bursts_then_refills_with_time() {
        let clock = Fake::default();
        let b = Bucket::new(10.0, 3, Arc::new(clock.clone()));
        assert!(b.allow() && b.allow() && b.allow());
        assert!(!b.allow());
        assert_eq!(b.wait_time(1), Duration::from_millis(100));
        clock.advance(Duration::from_millis(100));
        assert!(b.allow());
        clock.advance(Duration::from_secs(60));
        assert!((b.available() - 3.0).abs() < 1e-9, "capped at burst");
        assert!(!b.allow_n(4));
    }
}
