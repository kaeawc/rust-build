//! Sharded in-memory key/value store with optional TTL. Expiry is lazy on read plus
//! [`Memory::sweep`] for proactive cleanup; time comes from a [`Clock`](crate::clock::Clock).

use crate::clock::{SharedClock, lock};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SHARDS: usize = 16;

struct Entry<V> {
    value: V,
    expires: Option<Instant>,
}

pub struct Memory<V> {
    shards: Vec<Mutex<HashMap<String, Entry<V>>>>,
    clock: SharedClock,
}

impl<V: Clone> Memory<V> {
    pub fn new(clock: SharedClock) -> Self {
        Memory { shards: (0..SHARDS).map(|_| Mutex::default()).collect(), clock }
    }

    fn shard(&self, key: &str) -> &Mutex<HashMap<String, Entry<V>>> {
        let mut h = DefaultHasher::new();
        key.hash(&mut h);
        &self.shards[h.finish() as usize % SHARDS]
    }

    pub fn get(&self, key: &str) -> Option<V> {
        let now = self.clock.now();
        let mut shard = lock(self.shard(key));
        match shard.get(key) {
            Some(e) if e.expires.is_some_and(|t| now >= t) => {
                shard.remove(key);
                None
            }
            Some(e) => Some(e.value.clone()),
            None => None,
        }
    }

    /// Stores `value`; `ttl = None` never expires.
    pub fn set(&self, key: impl Into<String>, value: V, ttl: Option<Duration>) {
        let key = key.into();
        let expires = ttl.map(|d| self.clock.now() + d);
        lock(self.shard(&key)).insert(key, Entry { value, expires });
    }

    pub fn delete(&self, key: &str) -> bool {
        lock(self.shard(key)).remove(key).is_some()
    }

    /// Returns the cached value or computes, stores and returns it.
    pub fn get_or_set(&self, key: &str, ttl: Option<Duration>, compute: impl FnOnce() -> V) -> V {
        if let Some(v) = self.get(key) {
            return v;
        }
        let v = compute();
        self.set(key, v.clone(), ttl);
        v
    }

    /// Drops expired entries; returns how many.
    pub fn sweep(&self) -> usize {
        let now = self.clock.now();
        self.shards
            .iter()
            .map(|s| {
                let mut s = lock(s);
                let before = s.len();
                s.retain(|_, e| e.expires.is_none_or(|t| now < t));
                before - s.len()
            })
            .sum()
    }

    /// Drops every entry whose key starts with `prefix`; returns how many.
    pub fn sweep_prefix(&self, prefix: &str) -> usize {
        self.shards
            .iter()
            .map(|s| {
                let mut s = lock(s);
                let before = s.len();
                s.retain(|k, _| !k.starts_with(prefix));
                before - s.len()
            })
            .sum()
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| lock(s).len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Fake;
    use std::sync::Arc;

    #[test]
    fn ttl_expires_lazily_and_on_sweep() {
        let clock = Fake::default();
        let kv = Memory::new(Arc::new(clock.clone()));
        kv.set("a", 1, Some(Duration::from_secs(5)));
        kv.set("b", 2, Some(Duration::from_secs(10)));
        kv.set("c", 3, None);
        assert_eq!(kv.get("a"), Some(1));
        clock.advance(Duration::from_secs(5));
        assert_eq!(kv.get("a"), None);
        clock.advance(Duration::from_secs(5));
        assert_eq!(kv.sweep(), 1);
        assert_eq!(kv.len(), 1);
        assert_eq!(kv.get("c"), Some(3));
        kv.set("p:1", 1, None);
        kv.set("p:2", 2, None);
        assert_eq!(kv.sweep_prefix("p:"), 2);
        assert!(kv.delete("c"));
        assert!(kv.is_empty());
    }

    #[test]
    fn get_or_set_computes_once() {
        let kv = Memory::new(Arc::new(Fake::default()));
        let mut calls = 0;
        for _ in 0..3 {
            let v = kv.get_or_set("k", None, || {
                calls += 1;
                "v".to_string()
            });
            assert_eq!(v, "v");
        }
        assert_eq!(calls, 1);
    }
}
