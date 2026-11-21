//! Schema migrations, compatible with golang-migrate.
//!
//! Files live in `sql/migrations/NNNNNN_name.{up,down}.sql` and are embedded at compile time
//! (a unit test fails if the list and the directory drift). State is golang-migrate's
//! `schema_migrations(version bigint, dirty bool)`: a version is marked dirty before its file runs
//! and clean after, and a dirty database refuses to migrate until [`Migrator::force`].

use crate::BoxFut;
use std::sync::Mutex;

/// One migration's embedded SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub up: &'static str,
    pub down: &'static str,
}

macro_rules! migration {
    ($version:literal, $name:literal, $file:literal) => {
        Migration {
            version: $version,
            name: $name,
            up: include_str!(concat!("../../../sql/migrations/", $file, ".up.sql")),
            down: include_str!(concat!("../../../sql/migrations/", $file, ".down.sql")),
        }
    };
}

/// Every migration, ascending by version. Add new files here as well as on disk.
pub static MIGRATIONS: &[Migration] = &[migration!(1, "users", "000001_users"), migration!(2, "jobs", "000002_jobs")];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The applied version; `None` before the first migration.
    pub current: Option<i64>,
    pub dirty: bool,
    /// `(version, name)` of every known migration.
    pub migrations: Vec<(i64, &'static str)>,
}

impl Status {
    pub fn pending(&self) -> Vec<(i64, &'static str)> {
        self.migrations.iter().copied().filter(|(v, _)| self.current.is_none_or(|c| *v > c)).collect()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MigrateError {
    #[error("database is dirty at version {0}; fix it by hand, then `force` a version")]
    Dirty(i64),
    #[error("version {0} has no migration file")]
    UnknownVersion(i64),
    #[error("migration {version} ({name}) failed: {message}")]
    Failed { version: i64, name: &'static str, message: String },
    #[error("database: {0}")]
    Db(String),
}

/// Runs migrations. `rb migrate` and server boot take this, so tests use [`FakeMigrator`].
pub trait Migrator: Send + Sync {
    fn status(&self) -> BoxFut<'_, Result<Status, MigrateError>>;
    /// Applies every pending migration; returns the versions applied.
    fn up_all(&self) -> BoxFut<'_, Result<Vec<i64>, MigrateError>>;
    /// Applies the next migration, if any.
    fn up_one(&self) -> BoxFut<'_, Result<Option<i64>, MigrateError>>;
    /// Reverts the current migration; returns the version now current.
    fn down_one(&self) -> BoxFut<'_, Result<Option<i64>, MigrateError>>;
    /// Sets the version (or none) and clears `dirty` without running SQL.
    fn force(&self, version: Option<i64>) -> BoxFut<'_, Result<(), MigrateError>>;
}

/// Plans for the state machine shared by every implementation.
pub(crate) fn next_up(migrations: &[Migration], current: Option<i64>) -> Option<Migration> {
    migrations.iter().copied().find(|m| current.is_none_or(|c| m.version > c))
}

/// The migration to revert and the version that becomes current after it.
pub(crate) fn next_down(migrations: &[Migration], current: Option<i64>) -> Result<Option<(Migration, Option<i64>)>, MigrateError> {
    let Some(c) = current else { return Ok(None) };
    let idx = migrations.iter().position(|m| m.version == c).ok_or(MigrateError::UnknownVersion(c))?;
    let prev = idx.checked_sub(1).map(|i| migrations[i].version);
    Ok(Some((migrations[idx], prev)))
}

fn status_of(migrations: &[Migration], current: Option<i64>, dirty: bool) -> Status {
    Status { current, dirty, migrations: migrations.iter().map(|m| (m.version, m.name)).collect() }
}

/// In-memory migrator: tracks version and dirty state, runs no SQL. `fail_on` makes a version fail
/// (leaving the state dirty, like a real failure).
pub struct FakeMigrator {
    migrations: Vec<Migration>,
    state: Mutex<(Option<i64>, bool)>,
    pub fail_on: Option<i64>,
}

impl FakeMigrator {
    pub fn new(migrations: &[Migration]) -> Self {
        FakeMigrator { migrations: migrations.to_vec(), state: Mutex::new((None, false)), fail_on: None }
    }

    fn apply(&self, m: Migration, target: Option<i64>) -> Result<(), MigrateError> {
        let mut st = rb_core::clock::lock(&self.state);
        *st = (target, true);
        if self.fail_on == Some(m.version) {
            return Err(MigrateError::Failed { version: m.version, name: m.name, message: "injected failure".into() });
        }
        st.1 = false;
        Ok(())
    }

    fn current(&self) -> Result<Option<i64>, MigrateError> {
        let (v, dirty) = *rb_core::clock::lock(&self.state);
        if dirty {
            return Err(MigrateError::Dirty(v.unwrap_or(-1)));
        }
        Ok(v)
    }
}

impl Migrator for FakeMigrator {
    fn status(&self) -> BoxFut<'_, Result<Status, MigrateError>> {
        let (v, dirty) = *rb_core::clock::lock(&self.state);
        Box::pin(std::future::ready(Ok(status_of(&self.migrations, v, dirty))))
    }

    fn up_all(&self) -> BoxFut<'_, Result<Vec<i64>, MigrateError>> {
        let r = (|| {
            let mut applied = Vec::new();
            while let Some(m) = next_up(&self.migrations, self.current()?) {
                self.apply(m, Some(m.version))?;
                applied.push(m.version);
            }
            Ok(applied)
        })();
        Box::pin(std::future::ready(r))
    }

    fn up_one(&self) -> BoxFut<'_, Result<Option<i64>, MigrateError>> {
        let r = (|| match next_up(&self.migrations, self.current()?) {
            Some(m) => self.apply(m, Some(m.version)).map(|()| Some(m.version)),
            None => Ok(None),
        })();
        Box::pin(std::future::ready(r))
    }

    fn down_one(&self) -> BoxFut<'_, Result<Option<i64>, MigrateError>> {
        let r = (|| match next_down(&self.migrations, self.current()?)? {
            Some((m, prev)) => self.apply(m, prev).map(|()| prev),
            None => Ok(None),
        })();
        Box::pin(std::future::ready(r))
    }

    fn force(&self, version: Option<i64>) -> BoxFut<'_, Result<(), MigrateError>> {
        *rb_core::clock::lock(&self.state) = (version, false);
        Box::pin(std::future::ready(Ok(())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, Waker};

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        let mut f = std::pin::pin!(f);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    #[test]
    fn embedded_list_matches_the_directory() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sql/migrations");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("sql/migrations exists")
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter_map(|n| n.strip_suffix(".up.sql").map(str::to_string))
            .collect();
        on_disk.sort();
        let embedded: Vec<String> = MIGRATIONS.iter().map(|m| format!("{:06}_{}", m.version, m.name)).collect();
        assert_eq!(embedded, on_disk, "add new migration files to MIGRATIONS (and vice versa)");
        assert!(MIGRATIONS.windows(2).all(|w| w[0].version < w[1].version), "ascending versions");
        for name in &on_disk {
            assert!(dir.join(format!("{name}.down.sql")).exists(), "{name} has a down file");
        }
    }

    #[test]
    fn fake_walks_up_down_and_refuses_when_dirty() {
        let m = FakeMigrator::new(MIGRATIONS);
        assert_eq!(block_on(m.status()).map(|s| s.pending().len()), Ok(MIGRATIONS.len()));
        assert_eq!(block_on(m.up_one()), Ok(Some(1)));
        assert_eq!(block_on(m.up_all()), Ok(MIGRATIONS[1..].iter().map(|m| m.version).collect()));
        assert_eq!(block_on(m.up_one()), Ok(None));
        let last = MIGRATIONS.last().map(|m| m.version);
        assert_eq!(block_on(m.status()).map(|s| (s.current, s.pending().len())), Ok((last, 0)));
        assert_eq!(block_on(m.down_one()), Ok(Some(1)));
        assert_eq!(block_on(m.down_one()), Ok(None));
        assert_eq!(block_on(m.down_one()), Ok(None));

        let failing = FakeMigrator { fail_on: Some(2), ..FakeMigrator::new(MIGRATIONS) };
        assert!(matches!(block_on(failing.up_all()), Err(MigrateError::Failed { version: 2, .. })));
        assert_eq!(block_on(failing.up_one()), Err(MigrateError::Dirty(2)));
        assert_eq!(block_on(failing.force(Some(1))), Ok(()));
        assert_eq!(block_on(failing.status()).map(|s| (s.current, s.dirty)), Ok((Some(1), false)));
    }
}
