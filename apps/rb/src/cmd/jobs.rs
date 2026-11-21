//! `rb jobs`: enqueue, inspect and work background jobs.

use crate::backend::DbArgs;
use rb_core::clock::SharedClock;
use rb_jobs::{Periodic, PingArgs, Runner, RunnerConfig, Store, Worker};
use std::sync::Arc;
use std::time::Duration;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Enqueue a `ping` job (deduped per message per minute).
    Ping {
        message: String,
        #[command(flatten)]
        db: DbArgs,
    },
    /// Show job counts by state.
    Counts {
        #[command(flatten)]
        db: DbArgs,
    },
    /// Run workers until Ctrl-C (Postgres-backed with --database-url).
    Work {
        /// Also enqueue a `ping` every interval (a periodic job).
        #[arg(long, value_parser = super::duration)]
        heartbeat: Option<Duration>,
        #[command(flatten)]
        db: DbArgs,
    },
}

/// The workers every `rb` process knows.
pub fn runner(store: Arc<dyn Store>, clock: SharedClock) -> Runner {
    Runner::new(store, clock, RunnerConfig::default())
        .worker(Worker::new(|args: PingArgs| async move {
            tracing::info!(message = %args.message, "jobs.ping");
            Ok(())
        }))
        .worker(Worker::new(|args: rb_server::UserCreated| async move {
            tracing::info!(user_id = args.user_id, name = %args.name, "jobs.user_created");
            Ok(())
        }))
}

pub async fn run(c: Command) -> anyhow::Result<()> {
    let clock: SharedClock = Arc::new(rb_core::clock::System);
    match c {
        Command::Ping { message, db } => {
            let client = rb_jobs::Client::new(crate::backend::job_store(&db, clock.clone()).await?, clock);
            match client.insert(&PingArgs { message }).await? {
                Some(id) => println!("enqueued job {id}"),
                None => println!("duplicate: an identical ping is already queued this minute"),
            }
        }
        Command::Counts { db } => {
            let counts = crate::backend::job_store(&db, clock).await?.counts().await?;
            println!("{}", serde_json::to_string_pretty(&counts)?);
        }
        Command::Work { heartbeat, db } => {
            let store = crate::backend::job_store(&db, clock.clone()).await?;
            let mut r = runner(store, clock);
            if let Some(every) = heartbeat {
                r = r.periodic(Periodic::new("heartbeat", every, || PingArgs { message: "heartbeat".into() }));
            }
            tracing::info!("job workers running; Ctrl-C to drain and stop");
            r.run(rb_server::shutdown_signal()).await;
        }
    }
    Ok(())
}
