//! Concurrent load generation with live snapshots (port of golang-build's `cmd/loadgen` engine).
//!
//! [`start`] spawns `concurrency` workers that hit a [`Target`] until the duration elapses or
//! `max_requests` is reached. [`Run::snapshot`] is safe to call at any time for a live dashboard.

use crate::HttpClient;
use rb_core::clock::lock;
use rb_core::stats::Latencies;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Something to send load at. Returns the response status, or an error description.
pub trait Target: Send + Sync + 'static {
    fn hit(&self) -> impl Future<Output = Result<u16, String>> + Send;
}

/// GETs a URL with an [`HttpClient`].
pub struct HttpTarget {
    pub client: HttpClient,
    pub url: String,
}

impl Target for HttpTarget {
    async fn hit(&self) -> Result<u16, String> {
        self.client.get(&self.url).await.map(|r| r.status.as_u16()).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub concurrency: usize,
    pub duration: Duration,
    /// Stop after this many requests in total (useful for deterministic tests).
    pub max_requests: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config { concurrency: 8, duration: Duration::from_secs(5), max_requests: None }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub elapsed: Duration,
    pub total: u64,
    pub errors: u64,
    pub statuses: BTreeMap<u16, u64>,
    pub rps: f64,
    pub mean: Duration,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
    pub done: bool,
    pub last_error: Option<String>,
}

pub struct Run {
    started: Instant,
    finished: Mutex<Option<Instant>>,
    stats: Mutex<Latencies>,
    last_error: Mutex<Option<String>>,
    issued: AtomicU64,
    done: AtomicBool,
    finished_tx: tokio::sync::watch::Sender<bool>,
}

impl Run {
    pub fn snapshot(&self) -> Snapshot {
        let mut stats = lock(&self.stats).clone();
        let done = self.done.load(Ordering::SeqCst);
        let end = lock(&self.finished).unwrap_or_else(Instant::now);
        let elapsed = end.saturating_duration_since(self.started);
        let total = stats.count() as u64;
        Snapshot {
            elapsed,
            total,
            errors: stats.errors,
            statuses: stats.statuses.clone(),
            rps: if elapsed.is_zero() { 0.0 } else { total as f64 / elapsed.as_secs_f64() },
            mean: stats.mean(),
            p50: stats.percentile(50.0),
            p95: stats.percentile(95.0),
            p99: stats.percentile(99.0),
            done,
            last_error: lock(&self.last_error).clone(),
        }
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// Waits for every worker, then returns the final snapshot.
    pub async fn wait(&self) -> Snapshot {
        let mut rx = self.finished_tx.subscribe();
        let _ = rx.wait_for(|finished| *finished).await;
        self.snapshot()
    }

    /// Stops issuing new requests; in-flight ones still complete.
    pub fn stop(&self) {
        self.done.store(true, Ordering::SeqCst);
    }

    fn finish(&self) {
        self.done.store(true, Ordering::SeqCst);
        lock(&self.finished).get_or_insert_with(Instant::now);
        self.finished_tx.send_replace(true);
    }
}

/// Starts a run on the current tokio runtime.
pub fn start<T: Target>(target: Arc<T>, cfg: Config) -> Arc<Run> {
    let run = Arc::new(Run {
        started: Instant::now(),
        finished: Mutex::new(None),
        stats: Mutex::default(),
        last_error: Mutex::new(None),
        issued: AtomicU64::new(0),
        done: AtomicBool::new(false),
        finished_tx: tokio::sync::watch::channel(false).0,
    });
    let deadline = run.started + cfg.duration;
    let handles: Vec<_> = (0..cfg.concurrency.max(1))
        .map(|_| {
            let (run, target, max) = (run.clone(), target.clone(), cfg.max_requests);
            tokio::spawn(async move {
                // Batch samples locally and merge periodically: one lock per batch, not per request.
                let mut local = Latencies::default();
                while !run.done.load(Ordering::Relaxed) && Instant::now() < deadline {
                    if max.is_some_and(|m| run.issued.fetch_add(1, Ordering::Relaxed) >= m) {
                        break;
                    }
                    let t = Instant::now();
                    let result = target.hit().await;
                    let elapsed = t.elapsed();
                    match result {
                        Ok(status) => local.record(elapsed, Some(status)),
                        Err(e) => {
                            local.record(elapsed, None);
                            *lock(&run.last_error) = Some(e);
                        }
                    }
                    if local.count() >= 32 {
                        lock(&run.stats).merge(&local);
                        local = Latencies::default();
                    }
                }
                lock(&run.stats).merge(&local);
            })
        })
        .collect();
    let watcher = run.clone();
    tokio::spawn(async move {
        for w in handles {
            let _ = w.await;
        }
        watcher.finish();
    });
    run
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted {
        n: AtomicU64,
    }

    impl Target for Scripted {
        fn hit(&self) -> impl Future<Output = Result<u16, String>> + Send {
            let n = self.n.fetch_add(1, Ordering::Relaxed);
            async move {
                tokio::task::yield_now().await;
                match n % 10 {
                    0 => Err("boom".into()),
                    1 => Ok(500),
                    _ => Ok(200),
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stops_at_max_requests_and_tallies_statuses() {
        let run = start(
            Arc::new(Scripted { n: AtomicU64::new(0) }),
            Config { concurrency: 4, duration: Duration::from_secs(30), max_requests: Some(100) },
        );
        let snap = tokio::time::timeout(Duration::from_secs(5), run.wait()).await.expect("run finishes");
        assert!(snap.done);
        assert_eq!(snap.total, 100);
        assert_eq!(snap.errors, 10);
        assert_eq!(snap.statuses.get(&500), Some(&10));
        assert_eq!(snap.statuses.get(&200), Some(&80));
        assert_eq!(snap.last_error.as_deref(), Some("boom"));
        assert!(snap.p99 >= snap.p50);
    }

    #[tokio::test]
    async fn stop_ends_a_long_run() {
        let run = start(
            Arc::new(Scripted { n: AtomicU64::new(0) }),
            Config { concurrency: 2, duration: Duration::from_secs(60), max_requests: None },
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        run.stop();
        let snap = tokio::time::timeout(Duration::from_secs(5), run.wait()).await.expect("run stops");
        assert!(snap.done);
        assert!(snap.total > 0);
        assert!(snap.elapsed < Duration::from_secs(5));
    }
}
