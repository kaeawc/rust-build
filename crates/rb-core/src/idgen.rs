//! ID generation. [`Random`] for request IDs, [`Sequence`] for deterministic tests.

use crate::random::{Pcg32, Random as _};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Produces unique string IDs.
pub trait IdGen: Send + Sync + 'static {
    fn next_id(&self) -> String;
}

/// 128-bit random hex IDs from a PCG stream seeded per process. Unique, not unguessable: don't
/// use these as secrets.
#[derive(Debug)]
pub struct Random(Mutex<Pcg32>);

impl Default for Random {
    fn default() -> Self {
        Random(Mutex::new(Pcg32::from_entropy()))
    }
}

impl IdGen for Random {
    fn next_id(&self) -> String {
        let mut r = crate::clock::lock(&self.0);
        format!("{:016x}{:016x}", r.next_u64(), r.next_u64())
    }
}

/// `prefix-1`, `prefix-2`, ...
#[derive(Debug)]
pub struct Sequence {
    prefix: String,
    n: AtomicU64,
}

impl Sequence {
    pub fn new(prefix: impl Into<String>) -> Self {
        Sequence { prefix: prefix.into(), n: AtomicU64::new(0) }
    }
}

impl IdGen for Sequence {
    fn next_id(&self) -> String {
        format!("{}-{}", self.prefix, self.n.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn random_ids_are_32_hex_and_unique() {
        let g = Random::default();
        let ids: HashSet<String> = (0..1000).map(|_| g.next_id()).collect();
        assert_eq!(ids.len(), 1000);
        assert!(ids.iter().all(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())));
    }

    #[test]
    fn sequence_counts_from_one() {
        let g = Sequence::new("req");
        assert_eq!(g.next_id(), "req-1");
        assert_eq!(g.next_id(), "req-2");
    }
}
