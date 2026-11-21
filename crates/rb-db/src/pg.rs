//! Postgres: a deadpool pool of tokio-postgres connections, TLS through rustls/ring, and the
//! golang-migrate-compatible [`PgMigrator`].
//!
//! TLS follows the URL's `sslmode` (`disable`, `prefer` (default), `require`). Certificates are
//! verified against webpki roots, plus `RB_PG_CA` (a PEM file) if set; ALPN is `postgresql`.

use crate::BoxFut;
use crate::migrate::{MIGRATIONS, MigrateError, Migration, Migrator, Status, next_down, next_up};
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_postgres::tls::{ChannelBinding, MakeTlsConnect, TlsConnect};

pub use deadpool_postgres::{Object as PooledClient, Pool};
pub use tokio_postgres;

#[derive(Debug, thiserror::Error)]
pub enum PgError {
    #[error("invalid database url: {0}")]
    Url(String),
    #[error("tls: {0}")]
    Tls(#[from] rb_tls::Error),
    #[error("pool: {0}")]
    Pool(String),
    #[error("postgres: {0}")]
    Pg(#[from] tokio_postgres::Error),
}

/// `MakeTlsConnect` over a rustls `ClientConfig`.
#[derive(Clone)]
pub struct MakeRustls {
    connector: tokio_rustls::TlsConnector,
}

impl MakeRustls {
    pub fn new(cfg: Arc<rb_tls::rustls::ClientConfig>) -> Self {
        MakeRustls { connector: tokio_rustls::TlsConnector::from(cfg) }
    }

    /// webpki roots (+ the PEM CA in `RB_PG_CA`, if set), ALPN `postgresql`.
    pub fn from_env() -> Result<Self, PgError> {
        let trust = match std::env::var_os("RB_PG_CA") {
            Some(path) => {
                let pem = std::fs::read(&path).map_err(|e| PgError::Url(format!("RB_PG_CA: {e}")))?;
                rb_tls::Trust::WebPkiPlus(rb_tls::certs_from_pem(&pem, "RB_PG_CA")?)
            }
            None => rb_tls::Trust::WebPki,
        };
        let cfg = rb_tls::client_config(rb_tls::ClientOptions { trust, alpn: vec![b"postgresql".to_vec()], identity: None })?;
        Ok(MakeRustls::new(cfg))
    }
}

impl<S> MakeTlsConnect<S> for MakeRustls
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = RustlsStream<S>;
    type TlsConnect = RustlsConnect;
    type Error = io::Error;

    fn make_tls_connect(&mut self, domain: &str) -> io::Result<RustlsConnect> {
        let name = rb_tls::server_name(domain).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        Ok(RustlsConnect { connector: self.connector.clone(), name })
    }
}

pub struct RustlsConnect {
    connector: tokio_rustls::TlsConnector,
    name: rb_tls::pki::ServerName<'static>,
}

impl<S> TlsConnect<S> for RustlsConnect
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = RustlsStream<S>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<RustlsStream<S>>> + Send>>;

    fn connect(self, stream: S) -> Self::Future {
        Box::pin(async move { Ok(RustlsStream(Box::new(self.connector.connect(self.name, stream).await?))) })
    }
}

pub struct RustlsStream<S>(Box<tokio_rustls::client::TlsStream<S>>);

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for RustlsStream<S> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(self.get_mut().0.as_mut()).poll_read(cx, buf)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for RustlsStream<S> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(self.get_mut().0.as_mut()).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(self.get_mut().0.as_mut()).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(self.get_mut().0.as_mut()).poll_shutdown(cx)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> tokio_postgres::tls::TlsStream for RustlsStream<S> {
    fn channel_binding(&self) -> ChannelBinding {
        // SCRAM-SHA-256 works without it; only the -PLUS variant needs tls-server-end-point.
        ChannelBinding::none()
    }
}

/// Builds a pool for `url` (`postgres://user:pass@host:port/db?sslmode=...`).
pub fn pool(url: &str, max_size: usize) -> Result<Pool, PgError> {
    let cfg: tokio_postgres::Config = url.parse().map_err(|e: tokio_postgres::Error| PgError::Url(e.to_string()))?;
    let mgr = deadpool_postgres::Manager::from_config(
        cfg,
        MakeRustls::from_env()?,
        deadpool_postgres::ManagerConfig { recycling_method: deadpool_postgres::RecyclingMethod::Fast },
    );
    Pool::builder(mgr).max_size(max_size).build().map_err(|e| PgError::Pool(e.to_string()))
}

/// A dedicated (unpooled) connection, e.g. for `LISTEN`. The returned task drives the
/// connection and forwards async messages (notifications) to `on_message`.
pub async fn connect(
    url: &str,
    mut on_message: impl FnMut(tokio_postgres::AsyncMessage) + Send + 'static,
) -> Result<(tokio_postgres::Client, tokio::task::JoinHandle<()>), PgError> {
    let cfg: tokio_postgres::Config = url.parse().map_err(|e: tokio_postgres::Error| PgError::Url(e.to_string()))?;
    let (client, mut conn) = cfg.connect(MakeRustls::from_env()?).await?;
    let task = tokio::spawn(async move {
        loop {
            match std::future::poll_fn(|cx| conn.poll_message(cx)).await {
                Some(Ok(msg)) => on_message(msg),
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "postgres connection error");
                    break;
                }
                None => break,
            }
        }
    });
    Ok((client, task))
}

pub async fn ping(pool: &Pool) -> Result<(), PgError> {
    let c = pool.get().await.map_err(|e| PgError::Pool(e.to_string()))?;
    c.simple_query("SELECT 1").await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Migrations
// ---------------------------------------------------------------------------------------------

const LOCK_SALT: u32 = 1_486_364_155;

/// golang-migrate's advisory-lock id for `schema_migrations` in `schema` of `database`
/// (`database/util.go` GenerateAdvisoryLockId, as called by the pgx/v5 driver).
pub fn advisory_lock_id(database: &str, schema: &str) -> i64 {
    let key = format!("{schema}\0schema_migrations\0{database}");
    i64::from(crc32fast::hash(key.as_bytes()).wrapping_mul(LOCK_SALT))
}

/// Runs the embedded migrations against Postgres, golang-migrate style.
pub struct PgMigrator {
    pool: Pool,
    migrations: &'static [Migration],
}

impl PgMigrator {
    pub fn new(pool: Pool) -> Self {
        PgMigrator { pool, migrations: MIGRATIONS }
    }

    async fn client(&self) -> Result<PooledClient, MigrateError> {
        let c = self.pool.get().await.map_err(|e| MigrateError::Db(e.to_string()))?;
        c.batch_execute("CREATE TABLE IF NOT EXISTS schema_migrations (version bigint NOT NULL PRIMARY KEY, dirty boolean NOT NULL)")
            .await
            .map_err(db)?;
        Ok(c)
    }

    /// Runs `f` while holding golang-migrate's advisory lock.
    async fn locked<T>(&self, f: impl AsyncFnOnce(&mut PooledClient) -> Result<T, MigrateError>) -> Result<T, MigrateError> {
        let mut c = self.client().await?;
        let row = c.query_one("SELECT current_database(), current_schema()", &[]).await.map_err(db)?;
        let id = advisory_lock_id(row.get(0), row.get(1));
        c.execute("SELECT pg_advisory_lock($1)", &[&id]).await.map_err(db)?;
        let result = f(&mut c).await;
        let unlock = c.execute("SELECT pg_advisory_unlock($1)", &[&id]).await.map_err(db);
        let value = result?;
        unlock?;
        Ok(value)
    }

    async fn read(c: &PooledClient) -> Result<(Option<i64>, bool), MigrateError> {
        let row = c.query_opt("SELECT version, dirty FROM schema_migrations LIMIT 1", &[]).await.map_err(db)?;
        Ok(match row {
            Some(r) => {
                let v: i64 = r.get(0);
                (if v < 0 { None } else { Some(v) }, r.get(1))
            }
            None => (None, false),
        })
    }

    /// golang-migrate's SetVersion: truncate, then record `(version, dirty)` (a missing version is
    /// stored as -1 only while dirty).
    async fn write(c: &mut PooledClient, version: Option<i64>, dirty: bool) -> Result<(), MigrateError> {
        let tx = c.transaction().await.map_err(db)?;
        tx.batch_execute("TRUNCATE schema_migrations").await.map_err(db)?;
        if version.is_some() || dirty {
            tx.execute("INSERT INTO schema_migrations (version, dirty) VALUES ($1, $2)", &[&version.unwrap_or(-1), &dirty])
                .await
                .map_err(db)?;
        }
        tx.commit().await.map_err(db)
    }

    async fn clean(c: &PooledClient) -> Result<Option<i64>, MigrateError> {
        match Self::read(c).await? {
            (v, true) => Err(MigrateError::Dirty(v.unwrap_or(-1))),
            (v, false) => Ok(v),
        }
    }

    async fn run(c: &mut PooledClient, m: Migration, sql: &str, target: Option<i64>) -> Result<(), MigrateError> {
        Self::write(c, target, true).await?;
        c.batch_execute(sql).await.map_err(|e| MigrateError::Failed { version: m.version, name: m.name, message: e.to_string() })?;
        Self::write(c, target, false).await
    }
}

fn db(e: tokio_postgres::Error) -> MigrateError {
    MigrateError::Db(e.to_string())
}

impl Migrator for PgMigrator {
    fn status(&self) -> BoxFut<'_, Result<Status, MigrateError>> {
        Box::pin(async move {
            let c = self.client().await?;
            let (current, dirty) = Self::read(&c).await?;
            Ok(Status { current, dirty, migrations: self.migrations.iter().map(|m| (m.version, m.name)).collect() })
        })
    }

    fn up_all(&self) -> BoxFut<'_, Result<Vec<i64>, MigrateError>> {
        Box::pin(self.locked(async |c| {
            let mut applied = Vec::new();
            while let Some(m) = next_up(self.migrations, Self::clean(c).await?) {
                Self::run(c, m, m.up, Some(m.version)).await?;
                applied.push(m.version);
            }
            Ok(applied)
        }))
    }

    fn up_one(&self) -> BoxFut<'_, Result<Option<i64>, MigrateError>> {
        Box::pin(self.locked(async |c| match next_up(self.migrations, Self::clean(c).await?) {
            Some(m) => Self::run(c, m, m.up, Some(m.version)).await.map(|()| Some(m.version)),
            None => Ok(None),
        }))
    }

    fn down_one(&self) -> BoxFut<'_, Result<Option<i64>, MigrateError>> {
        Box::pin(self.locked(async |c| match next_down(self.migrations, Self::clean(c).await?)? {
            Some((m, prev)) => Self::run(c, m, m.down, prev).await.map(|()| prev),
            None => Ok(None),
        }))
    }

    fn force(&self, version: Option<i64>) -> BoxFut<'_, Result<(), MigrateError>> {
        Box::pin(async move {
            if let Some(v) = version
                && !self.migrations.iter().any(|m| m.version == v)
            {
                return Err(MigrateError::UnknownVersion(v));
            }
            let mut c = self.client().await?;
            Self::write(&mut c, version, false).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_id_matches_golang_migrate() {
        // golang-migrate: GenerateAdvisoryLockId("appdb", "public", "schema_migrations").
        let key = "public\0schema_migrations\0appdb";
        let expected = i64::from(crc32fast::hash(key.as_bytes()).wrapping_mul(1_486_364_155));
        assert_eq!(advisory_lock_id("appdb", "public"), expected);
        assert!(advisory_lock_id("appdb", "public") >= 0, "u32 range, as golang-migrate's fmt.Sprint(uint32)");
    }
}
