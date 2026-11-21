//! Executes jobs from a [`Store`](crate::Store).

use crate::{JobArgs, NewJob, RawJob, Store};
use rb_core::cache::BoxFut;
use rb_core::clock::SharedClock;
use rb_core::retry::Policy;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// What a worker tells the runner.
pub enum Outcome {
    Complete,
    /// Try again after this long without consuming an attempt.
    Snooze(Duration),
}

type Handler = Arc<dyn Fn(RawJob) -> BoxFut<'static, Result<Outcome, String>> + Send + Sync>;

/// A registered worker for one job kind.
#[derive(Clone)]
pub struct Worker {
    handler: Handler,
    timeout: Duration,
}

impl Worker {
    /// Wraps a typed async function: `Worker::new(|args: PingArgs| async move { ...; Ok(()) })`.
    pub fn new<A, F, Fut>(f: F) -> (&'static str, Worker)
    where
        A: JobArgs,
        F: Fn(A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        let f = Arc::new(f);
        let handler: Handler = Arc::new(move |job: RawJob| {
            let f = f.clone();
            Box::pin(async move {
                let args: A = serde_json::from_value(job.args).map_err(|e| format!("decoding args: {e}"))?;
                f(args).await.map(|()| Outcome::Complete)
            })
        });
        (A::KIND, Worker { handler, timeout: A::timeout() })
    }

    /// Like [`Worker::new`], but the function decides the [`Outcome`] (e.g. snooze).
    pub fn with_outcome<A, F, Fut>(f: F) -> (&'static str, Worker)
    where
        A: JobArgs,
        F: Fn(A, RawJob) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Outcome, String>> + Send + 'static,
    {
        let f = Arc::new(f);
        let handler: Handler = Arc::new(move |job: RawJob| {
            let f = f.clone();
            Box::pin(async move {
                let args: A = serde_json::from_value(job.args.clone()).map_err(|e| format!("decoding args: {e}"))?;
                f(args, job).await
            })
        });
        (A::KIND, Worker { handler, timeout: A::timeout() })
    }
}

/// A job enqueued on a fixed interval (once per interval across all nodes).
#[derive(Clone)]
pub struct Periodic {
    pub name: &'static str,
    pub every: Duration,
    pub make: Arc<dyn Fn(SystemTime) -> Result<NewJob, crate::JobError> + Send + Sync>,
}

impl Periodic {
    pub fn new<A: JobArgs>(name: &'static str, every: Duration, args: impl Fn() -> A + Send + Sync + 'static) -> Periodic {
        Periodic { name, every, make: Arc::new(move |now| NewJob::from_args(&args(), now)) }
    }
}

#[derive(Clone)]
pub struct RunnerConfig {
    /// `(queue, max concurrent jobs)`.
    pub queues: Vec<(&'static str, usize)>,
    pub retry: Policy,
    /// Wake up to look for work at least this often (notifications usually come sooner).
    pub poll_interval: Duration,
    /// Jobs running longer than this are assumed dead and made runnable again.
    pub rescue_after: Duration,
    /// Finished jobs are deleted after this long.
    pub retain_finished: Duration,
    /// How long running jobs get to finish on shutdown.
    pub shutdown_grace: Duration,
    pub worker_id: String,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        RunnerConfig {
            queues: vec![("default", 10)],
            // River's default: 25 attempts, backoff growing to an hour.
            retry: Policy {
                max_attempts: 25,
                initial: Duration::from_secs(1),
                max_delay: Duration::from_secs(3600),
                multiplier: 4.0,
                jitter: 0.1,
            },
            poll_interval: Duration::from_secs(1),
            rescue_after: Duration::from_secs(3600),
            retain_finished: Duration::from_secs(24 * 3600),
            shutdown_grace: Duration::from_secs(30),
            worker_id: format!("{}-{}", std::process::id(), rb_core::idgen::IdGen::next_id(&rb_core::idgen::Random::default())),
        }
    }
}

pub struct Runner {
    store: Arc<dyn Store>,
    clock: SharedClock,
    cfg: RunnerConfig,
    workers: HashMap<&'static str, Worker>,
    periodic: Vec<Periodic>,
}

impl Runner {
    pub fn new(store: Arc<dyn Store>, clock: SharedClock, cfg: RunnerConfig) -> Self {
        Runner { store, clock, cfg, workers: HashMap::new(), periodic: Vec::new() }
    }

    pub fn worker(mut self, (kind, worker): (&'static str, Worker)) -> Self {
        self.workers.insert(kind, worker);
        self
    }

    pub fn periodic(mut self, p: Periodic) -> Self {
        self.periodic.push(p);
        self
    }

    /// Runs until `shutdown` resolves, then stops fetching and drains in-flight jobs for up to
    /// `shutdown_grace`.
    pub async fn run(self, shutdown: impl Future<Output = ()>) {
        let this = Arc::new(self);
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let mut tasks = JoinSet::new();
        for (queue, limit) in this.cfg.queues.clone() {
            tasks.spawn(this.clone().queue_loop(queue, limit, stop_rx.clone()));
        }
        tasks.spawn(this.clone().maintenance_loop(stop_rx.clone()));
        shutdown.await;
        tracing::info!("jobs: shutting down");
        let _ = stop_tx.send(true);
        while tasks.join_next().await.is_some() {}
    }

    fn now(&self) -> SystemTime {
        UNIX_EPOCH + self.clock.unix()
    }

    async fn queue_loop(self: Arc<Self>, queue: &'static str, limit: usize, mut stop: tokio::sync::watch::Receiver<bool>) {
        let permits = Arc::new(Semaphore::new(limit.max(1)));
        let mut wake = self.store.wakeups();
        let mut running = JoinSet::new();
        loop {
            if *stop.borrow() {
                break;
            }
            let free = permits.available_permits();
            if free > 0 {
                match self.store.fetch(queue, free as u32, &self.cfg.worker_id).await {
                    Ok(jobs) => {
                        let fetched = jobs.len();
                        for job in jobs {
                            let Ok(permit) = permits.clone().try_acquire_owned() else { break };
                            let this = self.clone();
                            running.spawn(async move {
                                this.execute(job).await;
                                drop(permit);
                            });
                        }
                        if fetched == free {
                            // Likely more waiting: go again without sleeping.
                            continue;
                        }
                    }
                    Err(e) => tracing::warn!(queue, error = %e, "jobs: fetch failed"),
                }
            }
            tokio::select! {
                _ = stop.changed() => {}
                _ = wake.changed() => {}
                Some(_) = running.join_next(), if !running.is_empty() => {}
                () = tokio::time::sleep(self.cfg.poll_interval) => {}
            }
        }
        let drained = tokio::time::timeout(self.cfg.shutdown_grace, async { while running.join_next().await.is_some() {} }).await;
        if drained.is_err() {
            tracing::warn!(queue, remaining = running.len(), "jobs: grace period over; abandoning in-flight jobs (they will be rescued)");
            running.abort_all();
        }
    }

    async fn execute(&self, job: RawJob) {
        let span = tracing::info_span!("job", kind = %job.kind, queue = %job.queue, id = job.id, attempt = job.attempt);
        let _enter = span.enter();
        let Some(worker) = self.workers.get(job.kind.as_str()).cloned() else {
            let _ = self.store.fail(&job, &format!("no worker registered for kind {:?}", job.kind), self.backoff(job.attempt)).await;
            return;
        };
        drop(_enter);
        let result = tokio::time::timeout(worker.timeout, (worker.handler)(job.clone())).await;
        let outcome = match result {
            Ok(Ok(Outcome::Complete)) => self.store.complete(&job).await,
            Ok(Ok(Outcome::Snooze(d))) => self.store.snooze(&job, d).await,
            Ok(Err(e)) => {
                tracing::warn!(parent: &span, error = %e, "job failed");
                self.store.fail(&job, &e, self.backoff(job.attempt)).await
            }
            Err(_) => {
                tracing::warn!(parent: &span, timeout = ?worker.timeout, "job timed out");
                self.store.fail(&job, &format!("timed out after {:?}", worker.timeout), self.backoff(job.attempt)).await
            }
        };
        if let Err(e) = outcome {
            tracing::warn!(parent: &span, error = %e, "jobs: recording outcome failed");
        }
    }

    fn backoff(&self, attempt: i32) -> Duration {
        self.cfg.retry.base_delay(u32::try_from(attempt.max(1) - 1).unwrap_or(0))
    }

    async fn maintenance_loop(self: Arc<Self>, mut stop: tokio::sync::watch::Receiver<bool>) {
        loop {
            for p in &self.periodic {
                let now = self.now();
                match self.store.tick_periodic(p.name, now + p.every).await {
                    Ok(true) => match (p.make)(now) {
                        Ok(job) => {
                            if let Err(e) = self.store.insert(job).await {
                                tracing::warn!(name = p.name, error = %e, "jobs: periodic insert failed");
                            }
                        }
                        Err(e) => tracing::warn!(name = p.name, error = %e, "jobs: periodic args failed"),
                    },
                    Ok(false) => {}
                    Err(e) => tracing::warn!(name = p.name, error = %e, "jobs: periodic tick failed"),
                }
            }
            match self.store.rescue(self.cfg.rescue_after).await {
                Ok(n) if n > 0 => tracing::info!(rescued = n, "jobs: rescued stuck jobs"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "jobs: rescue failed"),
            }
            if let Err(e) = self.store.clean(self.cfg.retain_finished).await {
                tracing::warn!(error = %e, "jobs: clean failed");
            }
            tokio::select! {
                _ = stop.changed() => break,
                () = tokio::time::sleep(self.cfg.poll_interval) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Client, MemStore, PingArgs};
    use rb_core::clock::System;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Flaky {
        fail_times: u32,
    }
    impl JobArgs for Flaky {
        const KIND: &'static str = "flaky";
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runs_retries_and_periodic_then_drains() {
        let store: Arc<dyn Store> = Arc::new(MemStore::new(Arc::new(System)));
        let client = Client::new(store.clone(), Arc::new(System));
        let pings = Arc::new(AtomicU32::new(0));
        let attempts = Arc::new(AtomicU32::new(0));
        let (p, a) = (pings.clone(), attempts.clone());
        let cfg = RunnerConfig {
            poll_interval: Duration::from_millis(20),
            retry: Policy {
                initial: Duration::from_millis(1),
                max_delay: Duration::from_millis(5),
                jitter: 0.0,
                ..RunnerConfig::default().retry
            },
            ..RunnerConfig::default()
        };
        let runner = Runner::new(store.clone(), Arc::new(System), cfg)
            .worker(Worker::new(move |args: PingArgs| {
                let p = p.clone();
                async move {
                    assert!(args.message.starts_with("m"));
                    p.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }))
            .worker(Worker::new(move |args: Flaky| {
                let a = a.clone();
                async move {
                    let n = a.fetch_add(1, Ordering::SeqCst);
                    if n < args.fail_times { Err(format!("failure {n}")) } else { Ok(()) }
                }
            }))
            .periodic(Periodic::new("heartbeat", Duration::from_secs(3600), || PingArgs { message: "m-periodic".into() }));

        client.insert(&PingArgs { message: "m1".into() }).await.expect("insert");
        client.insert(&PingArgs { message: "m1".into() }).await.expect("insert (deduped)");
        client.insert(&PingArgs { message: "m2".into() }).await.expect("insert");
        client.insert(&Flaky { fail_times: 2 }).await.expect("insert");

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let run = tokio::spawn(runner.run(async {
            let _ = stop_rx.await;
        }));
        let done = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let c = client.counts().await.expect("counts");
                if c.completed == 4 {
                    return c;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("all jobs complete");
        assert_eq!(done.discarded, 0);
        assert_eq!(pings.load(Ordering::SeqCst), 3, "m1 once (deduped), m2, and the periodic tick");
        assert_eq!(attempts.load(Ordering::SeqCst), 3, "two failures then success");
        let _ = stop_tx.send(());
        tokio::time::timeout(Duration::from_secs(5), run).await.expect("drains").expect("joins");
    }
}
