//! In-memory [`Store`]: same state machine as Postgres, with time from a `Clock` so tests drive
//! retries, scheduling, uniqueness windows, periodic ticks and rescue without sleeping.

use crate::{Counts, JobError, JobId, NewJob, RawJob, Store};
use rb_core::cache::BoxFut;
use rb_core::clock::{SharedClock, lock};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Available,
    Running,
    Completed,
    Discarded,
}

#[derive(Clone, Debug)]
struct Row {
    job: NewJob,
    state: State,
    attempt: i32,
    scheduled_at: SystemTime,
    attempted_at: Option<SystemTime>,
    finalized_at: Option<SystemTime>,
    errors: Vec<String>,
}

#[derive(Default)]
struct Inner {
    next_id: JobId,
    rows: BTreeMap<JobId, Row>,
    periodic: HashMap<String, SystemTime>,
}

pub struct MemStore {
    clock: SharedClock,
    inner: Mutex<Inner>,
    wake: tokio::sync::watch::Sender<u64>,
}

impl MemStore {
    pub fn new(clock: SharedClock) -> Self {
        MemStore { clock, inner: Mutex::default(), wake: tokio::sync::watch::channel(0).0 }
    }

    fn now(&self) -> SystemTime {
        UNIX_EPOCH + self.clock.unix()
    }

    /// Errors recorded for a job (for tests and inspection).
    pub fn errors(&self, id: JobId) -> Vec<String> {
        lock(&self.inner).rows.get(&id).map(|r| r.errors.clone()).unwrap_or_default()
    }

    fn ready<T: Send + 'static>(v: T) -> BoxFut<'static, T> {
        Box::pin(std::future::ready(v))
    }
}

impl Store for MemStore {
    fn insert(&self, job: NewJob) -> BoxFut<'_, Result<Option<JobId>, JobError>> {
        let now = self.now();
        let mut inner = lock(&self.inner);
        if let Some(key) = &job.unique_key
            && inner.rows.values().any(|r| r.job.unique_key.as_ref() == Some(key))
        {
            return Self::ready(Ok(None));
        }
        inner.next_id += 1;
        let id = inner.next_id;
        let scheduled_at = job.scheduled_at.unwrap_or(now);
        inner.rows.insert(
            id,
            Row { job, state: State::Available, attempt: 0, scheduled_at, attempted_at: None, finalized_at: None, errors: Vec::new() },
        );
        drop(inner);
        self.wake.send_modify(|n| *n += 1);
        Self::ready(Ok(Some(id)))
    }

    fn fetch<'a>(&'a self, queue: &'a str, limit: u32, _worker: &'a str) -> BoxFut<'a, Result<Vec<RawJob>, JobError>> {
        let now = self.now();
        let mut inner = lock(&self.inner);
        let mut ready: Vec<(i16, SystemTime, JobId)> = inner
            .rows
            .iter()
            .filter(|(_, r)| r.state == State::Available && r.job.queue == queue && r.scheduled_at <= now)
            .map(|(id, r)| (r.job.priority, r.scheduled_at, *id))
            .collect();
        ready.sort();
        let mut out = Vec::new();
        for (_, _, id) in ready.into_iter().take(limit as usize) {
            if let Some(r) = inner.rows.get_mut(&id) {
                r.state = State::Running;
                r.attempt += 1;
                r.attempted_at = Some(now);
                out.push(RawJob {
                    id,
                    kind: r.job.kind.clone(),
                    queue: r.job.queue.clone(),
                    args: r.job.args.clone(),
                    attempt: r.attempt,
                    max_attempts: r.job.max_attempts,
                    keep_unique: r.job.keep_unique,
                });
            }
        }
        Self::ready(Ok(out))
    }

    fn complete(&self, job: &RawJob) -> BoxFut<'_, Result<(), JobError>> {
        let now = self.now();
        if let Some(r) = lock(&self.inner).rows.get_mut(&job.id).filter(|r| r.state == State::Running) {
            r.state = State::Completed;
            r.finalized_at = Some(now);
            if !r.job.keep_unique {
                r.job.unique_key = None;
            }
        }
        Self::ready(Ok(()))
    }

    fn fail<'a>(&'a self, job: &'a RawJob, error: &'a str, retry_in: Duration) -> BoxFut<'a, Result<(), JobError>> {
        let now = self.now();
        if let Some(r) = lock(&self.inner).rows.get_mut(&job.id).filter(|r| r.state == State::Running) {
            r.errors.push(format!("attempt {}: {error}", r.attempt));
            if r.attempt >= r.job.max_attempts {
                r.state = State::Discarded;
                r.finalized_at = Some(now);
                if !r.job.keep_unique {
                    r.job.unique_key = None;
                }
            } else {
                r.state = State::Available;
                r.scheduled_at = now + retry_in;
            }
        }
        Self::ready(Ok(()))
    }

    fn snooze(&self, job: &RawJob, delay: Duration) -> BoxFut<'_, Result<(), JobError>> {
        let now = self.now();
        if let Some(r) = lock(&self.inner).rows.get_mut(&job.id).filter(|r| r.state == State::Running) {
            r.state = State::Available;
            r.attempt -= 1;
            r.scheduled_at = now + delay;
        }
        Self::ready(Ok(()))
    }

    fn rescue(&self, older_than: Duration) -> BoxFut<'_, Result<u64, JobError>> {
        let now = self.now();
        let mut n = 0;
        for r in lock(&self.inner).rows.values_mut() {
            if r.state == State::Running && r.attempted_at.is_some_and(|t| t + older_than < now) {
                r.state = State::Available;
                r.scheduled_at = now;
                n += 1;
            }
        }
        if n > 0 {
            self.wake.send_modify(|w| *w += 1);
        }
        Self::ready(Ok(n))
    }

    fn tick_periodic<'a>(&'a self, name: &'a str, next: SystemTime) -> BoxFut<'a, Result<bool, JobError>> {
        let now = self.now();
        let mut inner = lock(&self.inner);
        let due = inner.periodic.get(name).is_none_or(|at| *at <= now);
        if due {
            inner.periodic.insert(name.to_string(), next);
        }
        Self::ready(Ok(due))
    }

    fn clean(&self, older_than: Duration) -> BoxFut<'_, Result<u64, JobError>> {
        let now = self.now();
        let mut inner = lock(&self.inner);
        let before = inner.rows.len();
        inner.rows.retain(|_, r| r.finalized_at.is_none_or(|t| t + older_than >= now));
        Self::ready(Ok((before - inner.rows.len()) as u64))
    }

    fn counts(&self) -> BoxFut<'_, Result<Counts, JobError>> {
        let mut c = Counts::default();
        for r in lock(&self.inner).rows.values() {
            match r.state {
                State::Available => c.available += 1,
                State::Running => c.running += 1,
                State::Completed => c.completed += 1,
                State::Discarded => c.discarded += 1,
            }
        }
        Self::ready(Ok(c))
    }

    fn wakeups(&self) -> tokio::sync::watch::Receiver<u64> {
        self.wake.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InsertOpts, JobArgs, PingArgs, Unique};
    use rb_core::clock::Fake;
    use std::sync::Arc;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        let mut f = std::pin::pin!(f);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        loop {
            if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Plain(u32);
    impl JobArgs for Plain {
        const KIND: &'static str = "plain";
        fn opts() -> InsertOpts {
            InsertOpts { max_attempts: 2, ..InsertOpts::default() }
        }
    }

    fn setup() -> (Fake, MemStore) {
        let clock = Fake::default();
        let store = MemStore::new(Arc::new(clock.clone()));
        (clock, store)
    }

    fn now(c: &Fake) -> SystemTime {
        UNIX_EPOCH + rb_core::clock::Clock::unix(c)
    }

    #[test]
    fn retries_with_backoff_then_discards() {
        let (clock, s) = setup();
        let id = block_on(s.insert(NewJob::from_args(&Plain(1), now(&clock)).expect("job"))).expect("insert").expect("id");
        let job = block_on(s.fetch("default", 10, "w")).expect("fetch").remove(0);
        assert_eq!((job.id, job.attempt), (id, 1));
        block_on(s.fail(&job, "boom", Duration::from_secs(10))).expect("fail");
        assert!(block_on(s.fetch("default", 10, "w")).expect("fetch").is_empty(), "not before the backoff");
        clock.advance(Duration::from_secs(10));
        let job = block_on(s.fetch("default", 10, "w")).expect("fetch").remove(0);
        assert_eq!(job.attempt, 2);
        block_on(s.fail(&job, "boom again", Duration::from_secs(10))).expect("fail");
        assert_eq!(block_on(s.counts()).expect("counts").discarded, 1, "attempts used up");
        assert_eq!(s.errors(id).len(), 2);
    }

    #[test]
    fn scheduled_priority_snooze_and_rescue() {
        let (clock, s) = setup();
        let later = now(&clock) + Duration::from_secs(60);
        let opts = InsertOpts { scheduled_at: Some(later), ..InsertOpts::default() };
        block_on(s.insert(NewJob::with_opts(&Plain(1), opts, now(&clock)).expect("job"))).expect("insert");
        let urgent = InsertOpts { priority: 0, ..InsertOpts::default() };
        block_on(s.insert(NewJob::with_opts(&Plain(2), InsertOpts::default(), now(&clock)).expect("job"))).expect("insert");
        block_on(s.insert(NewJob::with_opts(&Plain(3), urgent, now(&clock)).expect("job"))).expect("insert");
        let got: Vec<_> = block_on(s.fetch("default", 10, "w")).expect("fetch").iter().map(|j| j.args.clone()).collect();
        assert_eq!(got, vec![serde_json::json!(3), serde_json::json!(2)], "priority first; scheduled job not yet");

        let (clock2, s2) = setup();
        block_on(s2.insert(NewJob::from_args(&Plain(9), now(&clock2)).expect("job"))).expect("insert");
        let job = block_on(s2.fetch("default", 1, "w")).expect("fetch").remove(0);
        block_on(s2.snooze(&job, Duration::from_secs(5))).expect("snooze");
        clock2.advance(Duration::from_secs(5));
        let job = block_on(s2.fetch("default", 1, "w")).expect("fetch").remove(0);
        assert_eq!(job.attempt, 1, "snooze doesn't consume an attempt");
        clock2.advance(Duration::from_secs(301));
        assert_eq!(block_on(s2.rescue(Duration::from_secs(300))), Ok(1));
        assert_eq!(block_on(s2.fetch("default", 1, "w")).expect("fetch").len(), 1);
    }

    #[test]
    fn uniqueness_periodic_and_clean() {
        let (clock, s) = setup();
        let ping = PingArgs { message: "hi".into() };
        let first = block_on(s.insert(NewJob::from_args(&ping, now(&clock)).expect("job"))).expect("insert");
        assert!(first.is_some());
        assert_eq!(block_on(s.insert(NewJob::from_args(&ping, now(&clock)).expect("job"))), Ok(None), "deduped");
        let job = block_on(s.fetch("default", 1, "w")).expect("fetch").remove(0);
        block_on(s.complete(&job)).expect("complete");
        assert_eq!(block_on(s.insert(NewJob::from_args(&ping, now(&clock)).expect("job"))), Ok(None), "period key kept");

        let by_args = InsertOpts { unique: Unique::ByArgs, ..InsertOpts::default() };
        let a = block_on(s.insert(NewJob::with_opts(&Plain(5), by_args.clone(), now(&clock)).expect("job"))).expect("insert");
        assert!(a.is_some());
        assert_eq!(block_on(s.insert(NewJob::with_opts(&Plain(5), by_args.clone(), now(&clock)).expect("job"))), Ok(None));
        let job = block_on(s.fetch("default", 1, "w")).expect("fetch").remove(0);
        block_on(s.complete(&job)).expect("complete");
        assert!(
            block_on(s.insert(NewJob::with_opts(&Plain(5), by_args, now(&clock)).expect("job"))).expect("insert").is_some(),
            "args-only key released on finish"
        );

        let next = now(&clock) + Duration::from_secs(60);
        assert_eq!(block_on(s.tick_periodic("p", next)), Ok(true), "first tick runs");
        assert_eq!(block_on(s.tick_periodic("p", next)), Ok(false));
        clock.advance(Duration::from_secs(60));
        assert_eq!(block_on(s.tick_periodic("p", next + Duration::from_secs(60))), Ok(true));

        clock.advance(Duration::from_secs(3600));
        assert_eq!(block_on(s.clean(Duration::from_secs(600))), Ok(2));
    }
}
