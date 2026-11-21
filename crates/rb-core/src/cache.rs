//! Async string cache seam. [`MemoryCache`] (over [`kv::Memory`](crate::kv::Memory)) backs tests
//! and single-node runs; `rb_db::valkey::ValkeyCache` backs shared deployments.
//!
//! Invalidation uses generation counters rather than key scans: readers put the current value of a
//! `…:gen` key into their cache keys, writers [`Cache::incr`] it, and stale entries simply expire.

use crate::clock::SharedClock;
use crate::kv::Memory;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheError(pub String);

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cache: {}", self.0)
    }
}

impl std::error::Error for CacheError {}

pub trait Cache: Send + Sync + 'static {
    fn get<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<Option<String>, CacheError>>;
    /// Stores `value`; `ttl = None` never expires.
    fn set<'a>(&'a self, key: &'a str, value: String, ttl: Option<Duration>) -> BoxFut<'a, Result<(), CacheError>>;
    /// Atomically increments an integer key (missing = 0) and returns the new value.
    fn incr<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<i64, CacheError>>;
    fn ping(&self) -> BoxFut<'_, Result<(), CacheError>>;
}

pub struct MemoryCache {
    kv: Memory<String>,
    incr_lock: std::sync::Mutex<()>,
}

impl MemoryCache {
    pub fn new(clock: SharedClock) -> Self {
        MemoryCache { kv: Memory::new(clock), incr_lock: std::sync::Mutex::default() }
    }
}

impl Cache for MemoryCache {
    fn get<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<Option<String>, CacheError>> {
        Box::pin(std::future::ready(Ok(self.kv.get(key))))
    }

    fn set<'a>(&'a self, key: &'a str, value: String, ttl: Option<Duration>) -> BoxFut<'a, Result<(), CacheError>> {
        self.kv.set(key, value, ttl);
        Box::pin(std::future::ready(Ok(())))
    }

    fn incr<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<i64, CacheError>> {
        let _guard = crate::clock::lock(&self.incr_lock);
        let next = self.kv.get(key).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0) + 1;
        self.kv.set(key, next.to_string(), None);
        Box::pin(std::future::ready(Ok(next)))
    }

    fn ping(&self) -> BoxFut<'_, Result<(), CacheError>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Fake;
    use crate::testutil::block_on;
    use std::sync::Arc;

    #[test]
    fn get_set_ttl_and_incr() {
        let clock = Fake::default();
        let c = MemoryCache::new(Arc::new(clock.clone()));
        assert_eq!(block_on(c.get("k")), Ok(None));
        block_on(c.set("k", "v".into(), Some(Duration::from_secs(1)))).ok();
        assert_eq!(block_on(c.get("k")), Ok(Some("v".into())));
        clock.advance(Duration::from_secs(1));
        assert_eq!(block_on(c.get("k")), Ok(None));
        assert_eq!(block_on(c.incr("gen")), Ok(1));
        assert_eq!(block_on(c.incr("gen")), Ok(2));
        assert_eq!(block_on(c.ping()), Ok(()));
    }
}
