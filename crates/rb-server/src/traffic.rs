//! Per-route request counters, exposed at `/api/admin/traffic`. Keyed by the *matched* route
//! (`/api/users/{id}`, not `/api/users/42`) so cardinality stays bounded.

use rb_core::clock::lock;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct TrafficStat {
    pub method: String,
    pub path: String,
    pub count: u64,
    pub status2xx: u64,
    pub status4xx: u64,
    pub status5xx: u64,
    pub last_status: u16,
    pub last_nanos: u64,
    pub total_nanos: u64,
}

#[derive(Default)]
pub struct TrafficRecorder {
    stats: Mutex<BTreeMap<(String, String), TrafficStat>>,
}

impl TrafficRecorder {
    pub fn record(&self, method: &str, path: &str, status: u16, dur: Duration) {
        let mut stats = lock(&self.stats);
        let s = stats.entry((method.to_string(), path.to_string())).or_insert_with(|| TrafficStat {
            method: method.to_string(),
            path: path.to_string(),
            ..Default::default()
        });
        let nanos = u64::try_from(dur.as_nanos()).unwrap_or(u64::MAX);
        s.count += 1;
        s.last_status = status;
        s.last_nanos = nanos;
        s.total_nanos = s.total_nanos.saturating_add(nanos);
        match status {
            500.. => s.status5xx += 1,
            400..=499 => s.status4xx += 1,
            200..=299 => s.status2xx += 1,
            _ => {}
        }
    }

    /// Stats sorted by (path, method).
    pub fn snapshot(&self) -> Vec<TrafficStat> {
        let mut v: Vec<TrafficStat> = lock(&self.stats).values().cloned().collect();
        v.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_statuses_per_route() {
        let t = TrafficRecorder::default();
        t.record("GET", "/a", 200, Duration::from_millis(2));
        t.record("GET", "/a", 404, Duration::from_millis(3));
        t.record("GET", "/a", 503, Duration::from_millis(5));
        t.record("POST", "/a", 201, Duration::from_millis(1));
        let snap = t.snapshot();
        assert_eq!(snap.len(), 2);
        let get = &snap[0];
        assert_eq!((get.count, get.status2xx, get.status4xx, get.status5xx, get.last_status), (3, 1, 1, 1, 503));
        assert_eq!(get.total_nanos, 10_000_000);
    }
}
