//! Valkey (Redis-protocol) cache over a redis-rs `ConnectionManager`, which reconnects on its own.
//! `redis://` is plaintext; `rediss://` uses rustls with the ring provider and the OS root store.

use rb_core::cache::{BoxFut, Cache, CacheError};
use redis::aio::ConnectionManager;
use std::time::Duration;

pub struct ValkeyCache {
    conn: ConnectionManager,
}

fn err(e: redis::RedisError) -> CacheError {
    CacheError(e.to_string())
}

impl ValkeyCache {
    /// Connects to `url` (`redis://host:6379/0` or `rediss://...`).
    pub async fn connect(url: &str) -> Result<Self, CacheError> {
        // redis-rs builds its rustls config from the process default provider.
        let _ = rb_tls::rustls::crypto::ring::default_provider().install_default();
        let client = redis::Client::open(url).map_err(err)?;
        let conn = ConnectionManager::new(client).await.map_err(err)?;
        Ok(ValkeyCache { conn })
    }
}

impl Cache for ValkeyCache {
    fn get<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<Option<String>, CacheError>> {
        let mut conn = self.conn.clone();
        Box::pin(async move { redis::cmd("GET").arg(key).query_async(&mut conn).await.map_err(err) })
    }

    fn set<'a>(&'a self, key: &'a str, value: String, ttl: Option<Duration>) -> BoxFut<'a, Result<(), CacheError>> {
        let mut conn = self.conn.clone();
        Box::pin(async move {
            let mut cmd = redis::cmd("SET");
            cmd.arg(key).arg(value);
            if let Some(ttl) = ttl {
                cmd.arg("PX").arg(u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX).max(1));
            }
            cmd.query_async::<()>(&mut conn).await.map_err(err)
        })
    }

    fn incr<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<i64, CacheError>> {
        let mut conn = self.conn.clone();
        Box::pin(async move { redis::cmd("INCR").arg(key).query_async(&mut conn).await.map_err(err) })
    }

    fn ping(&self) -> BoxFut<'_, Result<(), CacheError>> {
        let mut conn = self.conn.clone();
        Box::pin(async move { redis::cmd("PING").query_async::<String>(&mut conn).await.map(drop).map_err(err) })
    }
}
