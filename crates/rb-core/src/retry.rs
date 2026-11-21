//! Exponential backoff with a cap and ± jitter, runtime-agnostic.
//!
//! The executor takes a [`Sleeper`], so production code passes a tokio-backed one
//! (`rb_client::TokioSleeper`) and tests pass [`Instant`], which records delays without waiting.

use crate::random::{Pcg32, Random};
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

/// Waits for a duration.
pub trait Sleeper: Send + Sync {
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> + Send;
}

/// Records each requested delay and returns immediately.
#[derive(Debug, Default)]
pub struct Instant {
    pub delays: Mutex<Vec<Duration>>,
}

impl Instant {
    pub fn recorded(&self) -> Vec<Duration> {
        crate::clock::lock(&self.delays).clone()
    }
}

impl Sleeper for Instant {
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> + Send {
        crate::clock::lock(&self.delays).push(d);
        std::future::ready(())
    }
}

/// Backoff policy.
#[derive(Clone, Debug)]
pub struct Policy {
    /// Total attempts including the first (1 = no retries).
    pub max_attempts: u32,
    pub initial: Duration,
    pub max_delay: Duration,
    pub multiplier: f64,
    /// Fraction of each delay to randomize by, in `[0, 1]`: 0.2 means ±20%.
    pub jitter: f64,
}

impl Default for Policy {
    fn default() -> Self {
        Policy { max_attempts: 4, initial: Duration::from_millis(100), max_delay: Duration::from_secs(5), multiplier: 2.0, jitter: 0.2 }
    }
}

impl Policy {
    /// The delay before retry number `retry` (0-based), before jitter.
    pub fn base_delay(&self, retry: u32) -> Duration {
        let factor = self.multiplier.powi(retry as i32);
        self.initial.mul_f64(factor).min(self.max_delay)
    }
}

/// Outcome classification for an attempt's error.
pub enum Verdict {
    Retry,
    Stop,
}

/// Runs an operation under a [`Policy`].
pub struct Executor<S> {
    policy: Policy,
    sleeper: S,
    rng: Mutex<Pcg32>,
}

impl<S: Sleeper> Executor<S> {
    pub fn new(policy: Policy, sleeper: S) -> Self {
        Executor { policy, sleeper, rng: Mutex::new(Pcg32::from_entropy()) }
    }

    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng = Mutex::new(Pcg32::seeded(seed));
        self
    }

    pub fn sleeper(&self) -> &S {
        &self.sleeper
    }

    fn jittered(&self, base: Duration) -> Duration {
        if self.policy.jitter <= 0.0 {
            return base;
        }
        let u = crate::clock::lock(&self.rng).next_f64() * 2.0 - 1.0; // [-1, 1)
        base.mul_f64((1.0 + u * self.policy.jitter.min(1.0)).max(0.0))
    }

    /// Calls `op` until it succeeds, `classify` says stop, or attempts run out.
    pub async fn run<T, E, F, Fut>(&self, mut op: F, classify: impl Fn(&E) -> Verdict) -> Result<T, E>
    where
        F: FnMut(u32) -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        let mut attempt = 0;
        loop {
            match op(attempt).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    attempt += 1;
                    if attempt >= self.policy.max_attempts.max(1) || matches!(classify(&e), Verdict::Stop) {
                        return Err(e);
                    }
                    self.sleeper.sleep(self.jittered(self.policy.base_delay(attempt - 1))).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::block_on;

    fn policy(jitter: f64) -> Policy {
        Policy { max_attempts: 5, initial: Duration::from_millis(100), max_delay: Duration::from_millis(500), multiplier: 2.0, jitter }
    }

    #[test]
    fn backs_off_exponentially_up_to_the_cap() {
        let ex = Executor::new(policy(0.0), Instant::default());
        let r: Result<(), &str> = block_on(ex.run(|_| async { Err("nope") }, |_| Verdict::Retry));
        assert_eq!(r, Err("nope"));
        let ms: Vec<u128> = ex.sleeper().recorded().iter().map(Duration::as_millis).collect();
        assert_eq!(ms, vec![100, 200, 400, 500]);
    }

    #[test]
    fn stops_on_success_and_on_stop_verdict() {
        let ex = Executor::new(policy(0.0), Instant::default());
        let r: Result<u32, &str> = block_on(ex.run(|n| async move { if n < 2 { Err("x") } else { Ok(n) } }, |_| Verdict::Retry));
        assert_eq!(r, Ok(2));
        assert_eq!(ex.sleeper().recorded().len(), 2);

        let ex = Executor::new(policy(0.0), Instant::default());
        let r: Result<(), &str> = block_on(ex.run(|_| async { Err("fatal") }, |_| Verdict::Stop));
        assert_eq!(r, Err("fatal"));
        assert!(ex.sleeper().recorded().is_empty());
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let ex = Executor::new(policy(0.2), Instant::default()).with_seed(9);
        let _: Result<(), ()> = block_on(ex.run(|_| async { Err(()) }, |_| Verdict::Retry));
        for (i, d) in ex.sleeper().recorded().iter().enumerate() {
            let base = policy(0.0).base_delay(i as u32).as_secs_f64();
            assert!((d.as_secs_f64() - base).abs() <= base * 0.2 + 1e-9, "{d:?} vs {base}");
        }
    }
}
