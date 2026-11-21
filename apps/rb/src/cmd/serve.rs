use crate::backend::DbArgs;
use crate::tlsargs::ServerTls;
use std::sync::Arc;
use std::time::Duration;

#[derive(clap::Args)]
pub struct Args {
    /// Address to listen on.
    #[arg(long, env = "RB_ADDR", default_value = "127.0.0.1:8080")]
    addr: String,
    /// HMAC secret for `/api/token` and `/api/me`.
    #[arg(long, env = "RB_TOKEN_SECRET", default_value = "dev-secret-change-me", hide_env_values = true)]
    token_secret: String,
    /// Grace period for in-flight requests on shutdown.
    #[arg(long, value_parser = super::duration, default_value = "10s")]
    shutdown_timeout: Duration,
    /// Apply pending migrations before serving (needs --database-url).
    #[arg(long, env = "RB_MIGRATE_ON_BOOT")]
    migrate: bool,
    /// Run background job workers in this process (Postgres-backed with --database-url).
    #[arg(long, env = "RB_JOBS")]
    jobs: bool,
    #[command(flatten)]
    db: DbArgs,
    #[command(flatten)]
    tls: ServerTls,
}

pub async fn run(a: Args) -> anyhow::Result<()> {
    let clock: rb_core::clock::SharedClock = Arc::new(rb_core::clock::System);
    let tls = a.tls.config(&[rb_tls::ALPN_H2, rb_tls::ALPN_HTTP11])?;

    if a.migrate {
        let applied = crate::backend::migrator(&a.db, false)?.up_all().await?;
        tracing::info!(?applied, "migrations applied");
    }
    // Independent startup work in parallel (Go's bootResources errgroup).
    let (cache, job_store) = tokio::try_join!(crate::backend::cache(&a.db), async {
        if a.jobs { crate::backend::job_store(&a.db, clock.clone()).await.map(Some) } else { Ok(None) }
    })?;
    let jobs = job_store.clone().map(|s| rb_jobs::Client::new(s, clock.clone()));

    let state = rb_server::AppState::new(rb_server::Config {
        token_secret: a.token_secret.into_bytes(),
        store: crate::backend::user_store(&a.db)?,
        cache,
        clock: clock.clone(),
        jobs,
        ..Default::default()
    });
    let app = rb_server::router(state.clone());
    let listener = tokio::net::TcpListener::bind(&a.addr).await?;
    let scheme = if tls.is_some() { "https" } else { "http" };
    tracing::info!(
        addr = %listener.local_addr()?,
        postgres = a.db.database_url.is_some(),
        valkey = a.db.valkey_url.is_some(),
        jobs = a.jobs,
        "listening on {scheme}://{} (h1 + h2{})",
        listener.local_addr()?,
        if tls.is_some() { "" } else { "c" }
    );

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let stopped = move || {
        let mut rx = stop_rx.clone();
        async move {
            let _ = rx.wait_for(|s| *s).await;
        }
    };
    let worker = job_store.map(|store| tokio::spawn(super::jobs::runner(store, clock.clone()).run(stopped())));

    let shutdown = {
        let state = state.clone();
        async move {
            rb_server::shutdown_signal().await;
            tracing::info!("shutdown requested; failing readiness and draining");
            state.set_shutting_down();
            let _ = stop_tx.send(true);
        }
    };
    rb_server::serve(listener, app, rb_server::ServeOptions { tls, shutdown_timeout: a.shutdown_timeout, ..Default::default() }, shutdown)
        .await?;
    if let Some(w) = worker {
        let _ = w.await;
    }
    Ok(())
}
