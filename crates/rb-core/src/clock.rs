//! Time source abstraction. [`System`] for production, [`Fake`] for deterministic tests.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A source of monotonic and wall-clock time.
pub trait Clock: Send + Sync + 'static {
    /// Monotonic time, for measuring durations and deadlines.
    fn now(&self) -> Instant;
    /// Wall-clock time as a duration since the Unix epoch, for timestamps and token expiry.
    fn unix(&self) -> Duration;
}

/// Shared handle to any clock.
pub type SharedClock = Arc<dyn Clock>;

/// The real clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct System;

impl Clock for System {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn unix(&self) -> Duration {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()
    }
}

/// A clock that only moves when told to. Clones share the same time.
#[derive(Clone, Debug)]
pub struct Fake {
    base: Instant,
    base_unix: Duration,
    offset: Arc<Mutex<Duration>>,
}

impl Fake {
    /// A fake clock whose wall time starts at `unix_secs`.
    pub fn at_unix(unix_secs: u64) -> Self {
        Fake { base: Instant::now(), base_unix: Duration::from_secs(unix_secs), offset: Arc::default() }
    }

    /// Moves time forward by `d`.
    pub fn advance(&self, d: Duration) {
        *lock(&self.offset) += d;
    }

    fn offset(&self) -> Duration {
        *lock(&self.offset)
    }
}

impl Default for Fake {
    fn default() -> Self {
        Fake::at_unix(1_700_000_000)
    }
}

impl Clock for Fake {
    fn now(&self) -> Instant {
        self.base + self.offset()
    }
    fn unix(&self) -> Duration {
        self.base_unix + self.offset()
    }
}

/// Locks a mutex, recovering the data if a panicking holder poisoned it.
pub fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_moves_only_when_advanced_and_clones_share_time() {
        let c = Fake::at_unix(100);
        let start = c.now();
        let other = c.clone();
        assert_eq!(c.now(), start);
        other.advance(Duration::from_secs(5));
        assert_eq!(c.now() - start, Duration::from_secs(5));
        assert_eq!(c.unix(), Duration::from_secs(105));
    }

    #[test]
    fn system_unix_is_after_2020() {
        assert!(System.unix().as_secs() > 1_577_836_800);
    }
}
