//! Background jobs, the River-shaped subset golang-build used, on our own small queue.
//!
//! - [`JobArgs`] types declare a `KIND` and insert options (queue, priority, attempts, uniqueness).
//! - [`Store`] is the persistence seam: [`MemStore`] (tests, single process, driven by a
//!   [`Clock`](rb_core::clock::Clock)) and `PgStore` (feature `postgres`: `FOR UPDATE SKIP LOCKED`
//!   claims, `LISTEN/NOTIFY` wakeups, transactional enqueue).
//! - [`Runner`] fetches per queue under a concurrency limit, retries failures with
//!   [`rb_core::retry::Policy`] backoff, runs periodic jobs, rescues jobs whose worker died, cleans
//!   finished rows, and drains on shutdown.
//!
//! Delivery is at-least-once: workers must be idempotent.

mod mem;
#[cfg(feature = "postgres")]
mod pg;
mod runner;

pub use mem::MemStore;
#[cfg(feature = "postgres")]
pub use pg::PgStore;
pub use runner::{Outcome, Periodic, Runner, RunnerConfig, Worker};

use rb_core::cache::BoxFut;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type JobId = i64;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum JobError {
    #[error("job store: {0}")]
    Store(String),
    #[error("job args: {0}")]
    Args(String),
}

/// Dedup policy, after River's `UniqueOpts{ByArgs, ByPeriod}`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Unique {
    #[default]
    None,
    /// At most one pending/running job with these args.
    ByArgs,
    /// At most one job with these args per time window (kept after completion).
    ByArgsPeriod(Duration),
}

#[derive(Clone, Debug)]
pub struct InsertOpts {
    pub queue: &'static str,
    /// Lower runs first.
    pub priority: i16,
    pub max_attempts: i32,
    pub unique: Unique,
    /// Run no earlier than this.
    pub scheduled_at: Option<SystemTime>,
}

impl Default for InsertOpts {
    fn default() -> Self {
        InsertOpts { queue: "default", priority: 1, max_attempts: 25, unique: Unique::None, scheduled_at: None }
    }
}

/// A job's arguments, stored as JSON.
pub trait JobArgs: Serialize + DeserializeOwned + Send + Sync + 'static {
    const KIND: &'static str;
    fn opts() -> InsertOpts {
        InsertOpts::default()
    }
    /// Per-attempt time limit.
    fn timeout() -> Duration {
        Duration::from_secs(120)
    }
}

/// A row to insert.
#[derive(Clone, Debug, PartialEq)]
pub struct NewJob {
    pub kind: String,
    pub queue: String,
    pub args: serde_json::Value,
    pub priority: i16,
    pub max_attempts: i32,
    pub scheduled_at: Option<SystemTime>,
    pub unique_key: Option<Vec<u8>>,
    /// Keep `unique_key` after the job finishes (period uniqueness).
    pub keep_unique: bool,
}

impl NewJob {
    pub fn from_args<A: JobArgs>(args: &A, now: SystemTime) -> Result<NewJob, JobError> {
        Self::with_opts(args, A::opts(), now)
    }

    pub fn with_opts<A: JobArgs>(args: &A, opts: InsertOpts, now: SystemTime) -> Result<NewJob, JobError> {
        let args = serde_json::to_value(args).map_err(|e| JobError::Args(e.to_string()))?;
        let at = opts.scheduled_at.unwrap_or(now);
        let (unique_key, keep_unique) = match opts.unique {
            Unique::None => (None, false),
            Unique::ByArgs => (Some(unique_key(A::KIND, opts.queue, &args, None)), false),
            Unique::ByArgsPeriod(p) => (Some(unique_key(A::KIND, opts.queue, &args, Some(window(at, p)))), true),
        };
        Ok(NewJob {
            kind: A::KIND.to_string(),
            queue: opts.queue.to_string(),
            args,
            priority: opts.priority,
            max_attempts: opts.max_attempts.max(1),
            scheduled_at: opts.scheduled_at,
            unique_key,
            keep_unique,
        })
    }
}

fn window(at: SystemTime, period: Duration) -> u64 {
    let secs = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    secs / period.as_secs().max(1)
}

/// sha256(kind ‖ queue ‖ canonical JSON args ‖ window). `serde_json::Value` objects are sorted
/// maps, so equal args always serialize identically.
fn unique_key(kind: &str, queue: &str, args: &serde_json::Value, window: Option<u64>) -> Vec<u8> {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    for part in [kind.as_bytes(), b"\0", queue.as_bytes(), b"\0", args.to_string().as_bytes(), b"\0"] {
        ctx.update(part);
    }
    if let Some(w) = window {
        ctx.update(&w.to_be_bytes());
    }
    ctx.finish().as_ref().to_vec()
}

/// A claimed job, handed to a worker.
#[derive(Clone, Debug, PartialEq)]
pub struct RawJob {
    pub id: JobId,
    pub kind: String,
    pub queue: String,
    pub args: serde_json::Value,
    /// 1-based attempt number of this run.
    pub attempt: i32,
    pub max_attempts: i32,
    pub keep_unique: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub available: u64,
    pub running: u64,
    pub completed: u64,
    pub discarded: u64,
    pub cancelled: u64,
}

/// Persistence for jobs. All methods are idempotent with respect to state checks (e.g. completing
/// a job that is no longer running does nothing).
pub trait Store: Send + Sync + 'static {
    /// Inserts a job; `None` when a unique job already exists.
    fn insert(&self, job: NewJob) -> BoxFut<'_, Result<Option<JobId>, JobError>>;
    /// Claims up to `limit` runnable jobs on `queue` for `worker`.
    fn fetch<'a>(&'a self, queue: &'a str, limit: u32, worker: &'a str) -> BoxFut<'a, Result<Vec<RawJob>, JobError>>;
    fn complete(&self, job: &RawJob) -> BoxFut<'_, Result<(), JobError>>;
    /// Records `error`; retries after `retry_in`, or discards when attempts are used up.
    fn fail<'a>(&'a self, job: &'a RawJob, error: &'a str, retry_in: Duration) -> BoxFut<'a, Result<(), JobError>>;
    /// Reschedules after `delay` without consuming an attempt.
    fn snooze(&self, job: &RawJob, delay: Duration) -> BoxFut<'_, Result<(), JobError>>;
    /// Makes jobs running longer than `older_than` runnable again; returns how many.
    fn rescue(&self, older_than: Duration) -> BoxFut<'_, Result<u64, JobError>>;
    /// Claims periodic job `name`'s tick if due (or never run), setting the next run to `next`.
    fn tick_periodic<'a>(&'a self, name: &'a str, next: SystemTime) -> BoxFut<'a, Result<bool, JobError>>;
    /// Deletes finished jobs older than `older_than`; returns how many.
    fn clean(&self, older_than: Duration) -> BoxFut<'_, Result<u64, JobError>>;
    fn counts(&self) -> BoxFut<'_, Result<Counts, JobError>>;
    /// Bumps whenever new work may be available (inserts, notifications).
    fn wakeups(&self) -> tokio::sync::watch::Receiver<u64>;
}

/// Enqueues typed jobs.
#[derive(Clone)]
pub struct Client {
    store: std::sync::Arc<dyn Store>,
    clock: rb_core::clock::SharedClock,
}

impl Client {
    pub fn new(store: std::sync::Arc<dyn Store>, clock: rb_core::clock::SharedClock) -> Self {
        Client { store, clock }
    }

    fn now(&self) -> SystemTime {
        UNIX_EPOCH + self.clock.unix()
    }

    pub async fn insert<A: JobArgs>(&self, args: &A) -> Result<Option<JobId>, JobError> {
        self.store.insert(NewJob::from_args(args, self.now())?).await
    }

    pub async fn insert_with<A: JobArgs>(&self, args: &A, opts: InsertOpts) -> Result<Option<JobId>, JobError> {
        self.store.insert(NewJob::with_opts(args, opts, self.now())?).await
    }

    pub async fn counts(&self) -> Result<Counts, JobError> {
        self.store.counts().await
    }

    pub fn store(&self) -> &std::sync::Arc<dyn Store> {
        &self.store
    }
}

/// `ping`: the job golang-build's River setup ran. Deduped by message per minute.
#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PingArgs {
    pub message: String,
}

impl JobArgs for PingArgs {
    const KIND: &'static str = "ping";
    fn opts() -> InsertOpts {
        InsertOpts { unique: Unique::ByArgsPeriod(Duration::from_secs(60)), ..InsertOpts::default() }
    }
    fn timeout() -> Duration {
        Duration::from_secs(30)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_keys_depend_on_args_and_window() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000 / 60 * 60);
        let a = NewJob::from_args(&PingArgs { message: "hi".into() }, t0).expect("job");
        let b = NewJob::from_args(&PingArgs { message: "hi".into() }, t0 + Duration::from_secs(30)).expect("job");
        let c = NewJob::from_args(&PingArgs { message: "yo".into() }, t0).expect("job");
        let d = NewJob::from_args(&PingArgs { message: "hi".into() }, t0 + Duration::from_secs(60)).expect("job");
        assert_eq!(a.unique_key, b.unique_key, "same window");
        assert_ne!(a.unique_key, c.unique_key, "different args");
        assert_ne!(a.unique_key, d.unique_key, "next window");
        assert!(a.keep_unique);
        assert_eq!(a.kind, "ping");
    }
}
