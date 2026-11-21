//! Builds storage backends from flags/env. Each backend needs its cargo feature; asking for one
//! the binary wasn't built with is an error that says which feature to enable.

#[cfg_attr(not(any(feature = "postgres", feature = "valkey")), allow(unused_imports))]
use anyhow::{Context as _, bail};
use rb_core::cache::Cache;
use rb_core::clock::SharedClock;
use rb_db::migrate::{FakeMigrator, MIGRATIONS, Migrator};
use rb_jobs::{MemStore, Store};
use std::sync::Arc;

#[derive(clap::Args, Clone, Debug, Default)]
pub struct DbArgs {
    /// Postgres URL, e.g. postgres://user:pass@localhost:5432/app?sslmode=disable (feature `postgres`).
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    pub database_url: Option<String>,
    /// Valkey URL, e.g. redis://localhost:6379 or rediss://… (feature `valkey`).
    #[arg(long, env = "VALKEY_URL", hide_env_values = true)]
    pub valkey_url: Option<String>,
    /// Postgres pool size.
    #[arg(long, default_value_t = 16)]
    pub db_pool_size: usize,
}

#[cfg(feature = "postgres")]
pub fn pool(a: &DbArgs) -> anyhow::Result<Option<rb_db::pg::Pool>> {
    a.database_url.as_deref().map(|url| rb_db::pg::pool(url, a.db_pool_size).context("postgres pool")).transpose()
}

fn require_postgres(a: &DbArgs) -> anyhow::Result<()> {
    if a.database_url.is_some() && !cfg!(feature = "postgres") {
        bail!("DATABASE_URL is set but this build lacks the `postgres` feature (cargo build -p rb --features postgres)");
    }
    Ok(())
}

pub async fn cache(a: &DbArgs) -> anyhow::Result<Option<Arc<dyn Cache>>> {
    let Some(url) = a.valkey_url.as_deref() else { return Ok(None) };
    #[cfg(feature = "valkey")]
    {
        let c = rb_db::valkey::ValkeyCache::connect(url).await.context("valkey")?;
        Ok(Some(Arc::new(c)))
    }
    #[cfg(not(feature = "valkey"))]
    {
        let _ = url;
        bail!("VALKEY_URL is set but this build lacks the `valkey` feature (cargo build -p rb --features valkey)")
    }
}

/// The user store: Postgres when `DATABASE_URL` is set, otherwise the seeded in-memory store.
pub fn user_store(a: &DbArgs) -> anyhow::Result<Arc<dyn rb_server::UserStore>> {
    require_postgres(a)?;
    #[cfg(feature = "postgres")]
    if let Some(p) = pool(a)? {
        return Ok(Arc::new(rb_server::PgUserStore(p)));
    }
    Ok(Arc::new(rb_server::MemoryStore::seeded(&["Ada", "Grace", "Linus", "Barbara", "Ken"])))
}

/// The job store: Postgres (with LISTEN) when `DATABASE_URL` is set, otherwise in-memory.
pub async fn job_store(a: &DbArgs, clock: SharedClock) -> anyhow::Result<Arc<dyn Store>> {
    require_postgres(a)?;
    #[cfg(feature = "postgres")]
    if let (Some(p), Some(url)) = (pool(a)?, a.database_url.as_deref()) {
        return Ok(Arc::new(rb_jobs::PgStore::new(p, Some(url)).await.context("job store")?));
    }
    Ok(Arc::new(MemStore::new(clock)))
}

pub fn migrator(a: &DbArgs, fake: bool) -> anyhow::Result<Box<dyn Migrator>> {
    if fake {
        return Ok(Box::new(FakeMigrator::new(MIGRATIONS)));
    }
    require_postgres(a)?;
    #[cfg(feature = "postgres")]
    if let Some(p) = pool(a)? {
        return Ok(Box::new(rb_db::pg::PgMigrator::new(p)));
    }
    bail!("set --database-url / DATABASE_URL (or pass --fake)")
}
