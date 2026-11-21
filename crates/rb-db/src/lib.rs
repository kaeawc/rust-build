//! Storage backends for rust-build, each behind an off-by-default feature.
//!
//! - [`migrate`]: the embedded `sql/migrations` list, the [`migrate::Migrator`] trait and
//!   [`migrate::FakeMigrator`] (always built); `PgMigrator` with `postgres`. The on-disk layout,
//!   `schema_migrations` table and advisory lock match golang-migrate, so the Go and Rust tools
//!   can share a database.
//! - `pg` (feature `postgres`): pooled tokio-postgres with TLS through our rustls/ring config.
//! - `valkey` (feature `valkey`): a [`rb_core::cache::Cache`] over a redis-rs connection manager.

pub mod migrate;
#[cfg(feature = "postgres")]
pub mod pg;
#[cfg(feature = "valkey")]
pub mod valkey;

pub use rb_core::cache::BoxFut;
