//! User storage behind a trait so handlers don't care where users live. [`MemoryStore`] backs
//! tests, demos and the desktop app's embedded server.

use rb_core::clock::lock;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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
