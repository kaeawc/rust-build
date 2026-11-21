//! Latency accumulation with percentiles, used by the load generator and dashboards.

use std::collections::BTreeMap;
use std::time::Duration;

/// Collects samples; percentiles sort lazily on read.
#[derive(Clone, Debug, Default)]
pub struct Latencies {
    samples: Vec<Duration>,
    sorted: bool,
    pub errors: u64,
    pub statuses: BTreeMap<u16, u64>,
}

impl Latencies {
    pub fn record(&mut self, d: Duration, status: Option<u16>) {
        self.samples.push(d);
        self.sorted = false;
        match status {
            Some(s) => *self.statuses.entry(s).or_default() += 1,
            None => self.errors += 1,
        }
    }

    pub fn count(&self) -> usize {
        self.samples.len()
    }

    /// Nearest-rank percentile, `p` in `[0, 100]`.
    pub fn percentile(&mut self, p: f64) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        if !self.sorted {
            self.samples.sort_unstable();
            self.sorted = true;
        }
        let rank = ((p / 100.0) * self.samples.len() as f64).ceil() as usize;
        self.samples[rank.clamp(1, self.samples.len()) - 1]
    }

    pub fn mean(&self) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        self.samples.iter().sum::<Duration>() / self.samples.len() as u32
    }

    pub fn merge(&mut self, other: &Latencies) {
        self.samples.extend_from_slice(&other.samples);
        self.sorted = false;
        self.errors += other.errors;
        for (k, v) in &other.statuses {
            *self.statuses.entry(*k).or_default() += v;
        }
    }
}

/// Parses `250ms`, `5s`, `2m`, `1h`, or a bare number of seconds.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let secs = match unit {
        "" | "s" => n,
        "ms" => n / 1000.0,
        "us" | "µs" => n / 1_000_000.0,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        _ => return None,
    };
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let mut l = Latencies::default();
        for ms in 1..=100 {
            l.record(Duration::from_millis(ms), Some(200));
        }
        l.record(Duration::from_millis(1), None);
        assert_eq!(l.percentile(50.0), Duration::from_millis(50));
        assert_eq!(l.percentile(99.0), Duration::from_millis(99));
        assert_eq!(l.percentile(100.0), Duration::from_millis(100));
        assert_eq!(l.statuses.get(&200), Some(&100));
        assert_eq!(l.errors, 1);
        assert_eq!(Latencies::default().percentile(50.0), Duration::ZERO);
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("250ms"), Some(Duration::from_millis(250)));
        assert_eq!(parse_duration("5s"), Some(Duration::from_secs(5)));
        assert_eq!(parse_duration("3"), Some(Duration::from_secs(3)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("1.5s"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("5x"), None);
    }
}
