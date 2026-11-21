//! Small deterministic PRNG (PCG32) for jitter and test data. Not for secrets.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

/// A source of pseudo-random numbers.
pub trait Random: Send {
    fn next_u32(&mut self) -> u32;

    fn next_u64(&mut self) -> u64 {
        (u64::from(self.next_u32()) << 32) | u64::from(self.next_u32())
    }

    /// Uniform in `[0, 1)`.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `[0, n)`; `n` must be non-zero.
    fn below(&mut self, n: u64) -> u64 {
        // Lemire's multiply-shift; the tiny bias is irrelevant for jitter and test data.
        ((u128::from(self.next_u64()) * u128::from(n)) >> 64) as u64
    }
}

/// PCG-XSH-RR 64/32.
#[derive(Clone, Debug)]
pub struct Pcg32 {
    state: u64,
    inc: u64,
}

impl Pcg32 {
    pub fn seeded(seed: u64) -> Self {
        let mut p = Pcg32 { state: 0, inc: (seed << 1) | 1 };
        p.next_u32();
        p.state = p.state.wrapping_add(seed);
        p.next_u32();
        p
    }

    /// Seeded from the process's per-run hasher keys (differs between runs).
    pub fn from_entropy() -> Self {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(std::process::id().into());
        Pcg32::seeded(h.finish())
    }
}

impl Random for Pcg32 {
    fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }
}

/// Picks one element uniformly.
pub fn pick<'a, T>(r: &mut impl Random, items: &'a [T]) -> Option<&'a T> {
    if items.is_empty() { None } else { items.get(r.below(items.len() as u64) as usize) }
}

/// Fisher–Yates shuffle in place.
pub fn shuffle<T>(r: &mut impl Random, items: &mut [T]) {
    for i in (1..items.len()).rev() {
        let j = r.below(i as u64 + 1) as usize;
        items.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let a: Vec<u32> = {
            let mut r = Pcg32::seeded(42);
            (0..8).map(|_| r.next_u32()).collect()
        };
        let b: Vec<u32> = {
            let mut r = Pcg32::seeded(42);
            (0..8).map(|_| r.next_u32()).collect()
        };
        assert_eq!(a, b);
        let mut c = Pcg32::seeded(43);
        assert_ne!(a[0], c.next_u32());
    }

    #[test]
    fn f64_and_below_stay_in_range() {
        let mut r = Pcg32::seeded(7);
        for _ in 0..10_000 {
            let f = r.next_f64();
            assert!((0.0..1.0).contains(&f));
            assert!(r.below(10) < 10);
        }
    }

    #[test]
    fn shuffle_is_a_permutation() {
        let mut r = Pcg32::seeded(1);
        let mut v: Vec<u32> = (0..50).collect();
        shuffle(&mut r, &mut v);
        let mut sorted = v.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>());
        assert_ne!(v, sorted);
        assert!(pick(&mut r, &v).is_some());
        assert!(pick::<u32>(&mut r, &[]).is_none());
    }
}
