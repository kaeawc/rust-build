//! Circuit breaker: after `threshold` consecutive failures the breaker opens and calls fail fast
//! with [`Error::Open`]; after `cooldown` one probe is let through (half-open), and its result
//! closes or re-opens the breaker.

use crate::clock::{SharedClock, lock};
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// Rejected without calling the operation.
    Open,
    /// The operation ran and failed.
    Inner(E),
}

impl<E: std::fmt::Display> std::fmt::Display for Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Open => f.write_str("circuit breaker is open"),
            Error::Inner(e) => e.fmt(f),
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for Error<E> {}

struct Inner {
    state: State,
    failures: u32,
    opened_at: Option<Instant>,
    probing: bool,
}

type Listener = Box<dyn Fn(State, State) + Send + Sync>;

pub struct Breaker {
    threshold: u32,
    cooldown: Duration,
    clock: SharedClock,
    inner: Mutex<Inner>,
    on_change: Option<Listener>,
}

impl Breaker {
    pub fn new(threshold: u32, cooldown: Duration, clock: SharedClock) -> Self {
        Breaker {
            threshold: threshold.max(1),
            cooldown,
            clock,
            inner: Mutex::new(Inner { state: State::Closed, failures: 0, opened_at: None, probing: false }),
            on_change: None,
        }
    }

    /// Called with `(from, to)` on every transition.
    pub fn on_state_change(mut self, f: impl Fn(State, State) + Send + Sync + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    pub fn state(&self) -> State {
        let mut inner = lock(&self.inner);
        self.refresh(&mut inner);
        inner.state
    }

    fn set(&self, inner: &mut Inner, to: State) {
        let from = inner.state;
        if from != to {
            inner.state = to;
            if let Some(f) = &self.on_change {
                f(from, to);
            }
        }
    }

    fn refresh(&self, inner: &mut Inner) {
        if inner.state == State::Open && inner.opened_at.is_some_and(|t| self.clock.now().duration_since(t) >= self.cooldown) {
            self.set(inner, State::HalfOpen);
        }
    }

    /// Admission check; returns whether this call is the half-open probe.
    fn admit(&self) -> Option<bool> {
        let mut inner = lock(&self.inner);
        self.refresh(&mut inner);
        match inner.state {
            State::Closed => Some(false),
            State::Open => None,
            State::HalfOpen if inner.probing => None,
            State::HalfOpen => {
                inner.probing = true;
                Some(true)
            }
        }
    }

    fn record(&self, probe: bool, ok: bool) {
        let mut inner = lock(&self.inner);
        if probe {
            inner.probing = false;
        }
        if ok {
            inner.failures = 0;
            self.set(&mut inner, State::Closed);
            return;
        }
        inner.failures += 1;
        if probe || inner.failures >= self.threshold {
            inner.opened_at = Some(self.clock.now());
            self.set(&mut inner, State::Open);
        }
    }

    /// Runs `op` if the breaker admits it. `is_failure` decides which errors count (e.g. a 404
    /// shouldn't trip the breaker).
    pub async fn call<T, E, Fut>(&self, op: impl FnOnce() -> Fut, is_failure: impl Fn(&E) -> bool) -> Result<T, Error<E>>
    where
        Fut: Future<Output = Result<T, E>>,
    {
        let probe = self.admit().ok_or(Error::Open)?;
        let result = op().await;
        let ok = match &result {
            Ok(_) => true,
            Err(e) => !is_failure(e),
        };
        self.record(probe, ok);
        result.map_err(Error::Inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Fake;
    use crate::testutil::block_on;
    use std::sync::Arc;

    fn fail(b: &Breaker) -> Result<(), Error<&'static str>> {
        block_on(b.call(|| async { Err::<(), _>("boom") }, |_| true))
    }
    fn succeed(b: &Breaker) -> Result<(), Error<&'static str>> {
        block_on(b.call(|| async { Ok::<_, &str>(()) }, |_| true))
    }

    #[test]
    fn opens_after_threshold_then_half_opens_after_cooldown() {
        let clock = Fake::default();
        let changes = Arc::new(Mutex::new(Vec::new()));
        let seen = changes.clone();
        let b = Breaker::new(3, Duration::from_secs(10), Arc::new(clock.clone()))
            .on_state_change(move |from, to| seen.lock().unwrap_or_else(|e| e.into_inner()).push((from, to)));

        for _ in 0..3 {
            assert_eq!(fail(&b), Err(Error::Inner("boom")));
        }
        assert_eq!(b.state(), State::Open);
        assert_eq!(succeed(&b), Err(Error::Open));

        clock.advance(Duration::from_secs(10));
        assert_eq!(b.state(), State::HalfOpen);
        assert_eq!(fail(&b), Err(Error::Inner("boom")));
        assert_eq!(b.state(), State::Open, "a failed probe re-opens");

        clock.advance(Duration::from_secs(10));
        assert_eq!(succeed(&b), Ok(()));
        assert_eq!(b.state(), State::Closed);
        let log = changes.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(log.first(), Some(&(State::Closed, State::Open)));
        assert_eq!(log.last(), Some(&(State::HalfOpen, State::Closed)));
    }

    #[test]
    fn non_failures_do_not_trip_and_success_resets() {
        let b = Breaker::new(2, Duration::from_secs(1), Arc::new(Fake::default()));
        for _ in 0..5 {
            let r = block_on(b.call(|| async { Err::<(), _>(404) }, |code| *code >= 500));
            assert_eq!(r, Err(Error::Inner(404)));
        }
        assert_eq!(b.state(), State::Closed);
        let _ = fail(&b);
        let _ = succeed(&b);
        let _ = fail(&b);
        assert_eq!(b.state(), State::Closed, "streak reset by the success");
    }
}
