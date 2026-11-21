//! User storage behind a trait so handlers don't care where users live. [`MemoryStore`] backs
//! tests, demos and the desktop app's embedded server.

use rb_core::clock::lock;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

pub use rb_core::cache::BoxFut;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NewUser {
    pub name: String,
}

#[derive(Debug)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub trait UserStore: Send + Sync + 'static {
    /// Up to `limit` users with `id > after`, ascending by id.
    fn list(&self, after: Option<i64>, limit: usize) -> BoxFut<'_, Result<Vec<User>, StoreError>>;
    fn create(&self, user: NewUser) -> BoxFut<'_, Result<User, StoreError>>;
    fn ping(&self) -> BoxFut<'_, Result<(), StoreError>>;
}

#[derive(Default)]
pub struct MemoryStore {
    users: Mutex<Vec<User>>,
}

impl MemoryStore {
    pub fn seeded(names: &[&str]) -> Self {
        let users = names.iter().zip(1..).map(|(n, id)| User { id, name: (*n).to_string() }).collect();
        MemoryStore { users: Mutex::new(users) }
    }
}

impl UserStore for MemoryStore {
    fn list(&self, after: Option<i64>, limit: usize) -> BoxFut<'_, Result<Vec<User>, StoreError>> {
        let users = lock(&self.users);
        let out = users.iter().filter(|u| after.is_none_or(|a| u.id > a)).take(limit).cloned().collect();
        Box::pin(std::future::ready(Ok(out)))
    }

    fn create(&self, user: NewUser) -> BoxFut<'_, Result<User, StoreError>> {
        let mut users = lock(&self.users);
        let id = users.last().map_or(1, |u| u.id + 1);
        let u = User { id, name: user.name };
        users.push(u.clone());
        Box::pin(std::future::ready(Ok(u)))
    }

    fn ping(&self) -> BoxFut<'_, Result<(), StoreError>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// Users in Postgres (feature `postgres`), mirroring the sqlc queries in `sql/queries/users.sql`.
#[cfg(feature = "postgres")]
pub struct PgUserStore(pub rb_db::pg::Pool);

#[cfg(feature = "postgres")]
impl PgUserStore {
    async fn client(&self) -> Result<rb_db::pg::PooledClient, StoreError> {
        self.0.get().await.map_err(|e| StoreError(e.to_string()))
    }
}

#[cfg(feature = "postgres")]
fn pg_err(e: rb_db::pg::tokio_postgres::Error) -> StoreError {
    StoreError(e.to_string())
}

#[cfg(feature = "postgres")]
impl UserStore for PgUserStore {
    fn list(&self, after: Option<i64>, limit: usize) -> BoxFut<'_, Result<Vec<User>, StoreError>> {
        Box::pin(async move {
            let c = self.client().await?;
            let stmt = c.prepare_cached("SELECT id, name FROM users WHERE id > $1 ORDER BY id LIMIT $2").await.map_err(pg_err)?;
            let limit = i64::try_from(limit).unwrap_or(i64::MAX);
            let rows = c.query(&stmt, &[&after.unwrap_or(0), &limit]).await.map_err(pg_err)?;
            Ok(rows.iter().map(|r| User { id: r.get(0), name: r.get(1) }).collect())
        })
    }

    fn create(&self, user: NewUser) -> BoxFut<'_, Result<User, StoreError>> {
        Box::pin(async move {
            let c = self.client().await?;
            let stmt = c.prepare_cached("INSERT INTO users (name) VALUES ($1) RETURNING id, name").await.map_err(pg_err)?;
            let r = c.query_one(&stmt, &[&user.name]).await.map_err(pg_err)?;
            Ok(User { id: r.get(0), name: r.get(1) })
        })
    }

    fn ping(&self) -> BoxFut<'_, Result<(), StoreError>> {
        Box::pin(async move { rb_db::pg::ping(&self.0).await.map_err(|e| StoreError(e.to_string())) })
    }
}
