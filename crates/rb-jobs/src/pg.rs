//! Postgres [`Store`]: claims with `FOR UPDATE SKIP LOCKED`, wakes workers with `LISTEN/NOTIFY`
//! (delivered only on commit), and supports enqueueing inside the caller's transaction.

use crate::{Counts, JobError, JobId, NewJob, RawJob, Store};
use rb_core::cache::BoxFut;
use rb_db::pg::tokio_postgres::{AsyncMessage, GenericClient};
use rb_db::pg::{Pool, PooledClient};
use std::time::{Duration, SystemTime};

const CHANNEL: &str = "rb_job";

pub struct PgStore {
    pool: Pool,
    wake: tokio::sync::watch::Sender<u64>,
    _listener: Option<tokio::task::JoinHandle<()>>,
}

fn err(e: impl std::fmt::Display) -> JobError {
    JobError::Store(e.to_string())
}

const INSERT: &str = "INSERT INTO rb_job (kind, queue, args, priority, max_attempts, scheduled_at, unique_key, unique_keep)
     VALUES ($1, $2, $3, $4, $5, coalesce($6, now()), $7, $8)
     ON CONFLICT (unique_key) WHERE unique_key IS NOT NULL DO NOTHING
     RETURNING id";

impl PgStore {
    /// A store on `pool`. With `listen_url`, a dedicated connection `LISTEN`s for new jobs so
    /// workers wake immediately instead of on the next poll.
    pub async fn new(pool: Pool, listen_url: Option<&str>) -> Result<Self, JobError> {
        let wake = tokio::sync::watch::channel(0).0;
        let listener = match listen_url {
            Some(url) => {
                let tx = wake.clone();
                let (client, task) = rb_db::pg::connect(url, move |msg| {
                    if let AsyncMessage::Notification(_) = msg {
                        tx.send_modify(|n| *n += 1);
                    }
                })
                .await
                .map_err(err)?;
                client.batch_execute(&format!("LISTEN {CHANNEL}")).await.map_err(err)?;
                // Keep the client alive for as long as the connection task runs.
                Some(tokio::spawn(async move {
                    let _client = client;
                    let _ = task.await;
                }))
            }
            None => None,
        };
        Ok(PgStore { pool, wake, _listener: listener })
    }

    async fn client(&self) -> Result<PooledClient, JobError> {
        self.pool.get().await.map_err(err)
    }

    /// Enqueues inside an open transaction: the job exists (and workers are notified) only if the
    /// transaction commits.
    pub async fn insert_in(tx: &impl GenericClient, job: &NewJob) -> Result<Option<JobId>, JobError> {
        let row = tx
            .query_opt(
                INSERT,
                &[&job.kind, &job.queue, &job.args, &job.priority, &job.max_attempts, &job.scheduled_at, &job.unique_key, &job.keep_unique],
            )
            .await
            .map_err(err)?;
        if row.is_some() {
            tx.execute("SELECT pg_notify($1, $2)", &[&CHANNEL, &job.queue]).await.map_err(err)?;
        }
        Ok(row.map(|r| r.get(0)))
    }
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

impl Store for PgStore {
    fn insert(&self, job: NewJob) -> BoxFut<'_, Result<Option<JobId>, JobError>> {
        Box::pin(async move {
            let c = self.client().await?;
            PgStore::insert_in(&**c, &job).await
        })
    }

    fn fetch<'a>(&'a self, queue: &'a str, limit: u32, worker: &'a str) -> BoxFut<'a, Result<Vec<RawJob>, JobError>> {
        Box::pin(async move {
            let c = self.client().await?;
            let rows = c
                .query(
                    "WITH next AS (
                         SELECT id FROM rb_job
                         WHERE state = 'available' AND queue = $1 AND scheduled_at <= now()
                         ORDER BY priority, scheduled_at, id
                         LIMIT $2
                         FOR UPDATE SKIP LOCKED
                     )
                     UPDATE rb_job j
                     SET state = 'running', attempt = j.attempt + 1, attempted_at = now(), attempted_by = $3
                     FROM next WHERE j.id = next.id
                     RETURNING j.id, j.kind, j.queue, j.args, j.attempt, j.max_attempts, j.unique_keep",
                    &[&queue, &i64::from(limit), &worker],
                )
                .await
                .map_err(err)?;
            Ok(rows
                .iter()
                .map(|r| RawJob {
                    id: r.get(0),
                    kind: r.get(1),
                    queue: r.get(2),
                    args: r.get(3),
                    attempt: r.get(4),
                    max_attempts: r.get(5),
                    keep_unique: r.get(6),
                })
                .collect())
        })
    }

    fn complete(&self, job: &RawJob) -> BoxFut<'_, Result<(), JobError>> {
        let id = job.id;
        Box::pin(async move {
            let c = self.client().await?;
            // Args-only keys are released on finish; period keys stay so the window still dedups.
            c.execute(
                "UPDATE rb_job SET state = 'completed', finalized_at = now(),
                     unique_key = CASE WHEN unique_keep THEN unique_key END
                 WHERE id = $1 AND state = 'running'",
                &[&id],
            )
            .await
            .map_err(err)?;
            Ok(())
        })
    }

    fn fail<'a>(&'a self, job: &'a RawJob, error: &'a str, retry_in: Duration) -> BoxFut<'a, Result<(), JobError>> {
        Box::pin(async move {
            let c = self.client().await?;
            c.execute(
                "UPDATE rb_job SET
                     errors = errors || jsonb_build_object('attempt', attempt, 'at', now(), 'error', $2::text),
                     state = CASE WHEN attempt >= max_attempts THEN 'discarded' ELSE 'available' END,
                     scheduled_at = CASE WHEN attempt >= max_attempts THEN scheduled_at
                                         ELSE now() + make_interval(secs => $3) END,
                     finalized_at = CASE WHEN attempt >= max_attempts THEN now() END,
                     unique_key = CASE WHEN attempt >= max_attempts AND NOT unique_keep THEN NULL
                                       ELSE unique_key END
                 WHERE id = $1 AND state = 'running'",
                &[&job.id, &error, &secs(retry_in)],
            )
            .await
            .map_err(err)?;
            Ok(())
        })
    }

    fn snooze(&self, job: &RawJob, delay: Duration) -> BoxFut<'_, Result<(), JobError>> {
        let id = job.id;
        Box::pin(async move {
            let c = self.client().await?;
            c.execute(
                "UPDATE rb_job SET state = 'available', attempt = attempt - 1, scheduled_at = now() + make_interval(secs => $2)
                 WHERE id = $1 AND state = 'running'",
                &[&id, &secs(delay)],
            )
            .await
            .map_err(err)?;
            Ok(())
        })
    }

    fn rescue(&self, older_than: Duration) -> BoxFut<'_, Result<u64, JobError>> {
        Box::pin(async move {
            let c = self.client().await?;
            c.execute(
                "UPDATE rb_job SET state = 'available', scheduled_at = now()
                 WHERE state = 'running' AND attempted_at < now() - make_interval(secs => $1)",
                &[&secs(older_than)],
            )
            .await
            .map_err(err)
        })
    }

    fn tick_periodic<'a>(&'a self, name: &'a str, next: SystemTime) -> BoxFut<'a, Result<bool, JobError>> {
        Box::pin(async move {
            let c = self.client().await?;
            // Insert-or-claim in one statement: exactly one node wins each tick.
            let row = c
                .query_opt(
                    "INSERT INTO rb_periodic (name, next_run_at) VALUES ($1, $2)
                     ON CONFLICT (name) DO UPDATE SET next_run_at = EXCLUDED.next_run_at
                         WHERE rb_periodic.next_run_at <= now()
                     RETURNING name",
                    &[&name, &next],
                )
                .await
                .map_err(err)?;
            Ok(row.is_some())
        })
    }

    fn clean(&self, older_than: Duration) -> BoxFut<'_, Result<u64, JobError>> {
        Box::pin(async move {
            let c = self.client().await?;
            c.execute("DELETE FROM rb_job WHERE finalized_at < now() - make_interval(secs => $1)", &[&secs(older_than)]).await.map_err(err)
        })
    }

    fn counts(&self) -> BoxFut<'_, Result<Counts, JobError>> {
        Box::pin(async move {
            let c = self.client().await?;
            let rows = c.query("SELECT state, count(*) FROM rb_job GROUP BY state", &[]).await.map_err(err)?;
            let mut out = Counts::default();
            for r in rows {
                let n = u64::try_from(r.get::<_, i64>(1)).unwrap_or(0);
                match r.get::<_, &str>(0) {
                    "available" => out.available = n,
                    "running" => out.running = n,
                    "completed" => out.completed = n,
                    "discarded" => out.discarded = n,
                    "cancelled" => out.cancelled = n,
                    _ => {}
                }
            }
            Ok(out)
        })
    }

    fn wakeups(&self) -> tokio::sync::watch::Receiver<u64> {
        self.wake.subscribe()
    }
}
