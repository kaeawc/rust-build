# Postgres, Valkey and migrations for rust-build

Research date: 2026-11-21. Toolchain: Rust 1.99.1, edition 2024, tokio 1.54, axum 0.8.9. Status:
recommendation only. Nothing is implemented and no crate has been changed.

## What we are porting

| golang-build | What it does | Rust equivalent needed |
| --- | --- | --- |
| `internal/db` (pgx/v5 + sqlc v2) | `DBTX` interface. sqlc-generated `GetUsers :many` and `CreateUser :one` from `sql/queries/users.sql`. | pool, two typed queries, a `UserStore` impl |
| `internal/db/migrate.go`, `cmd/migrate` (golang-migrate v4, `pgx5://`) | `Migrator` interface with `Status/UpAll/UpOne/DownOne`, plus a `FakeMigrator`. Reads `sql/migrations/00000N_name.{up,down}.sql` (4 pairs). | `rb migrate up\|down\|status` with a fake |
| `internal/cache` (valkey-go) | `Cache{Get, Set(ttl), Close}` | async cache trait and a Valkey impl |

Note from the Go code: migrations must use a **direct**, not pooled, connection. Neon's pooler
returns NULL for `CURRENT_SCHEMA()`. Keep this rule.

## Method

- Each candidate went into a copy of the workspace: today's `Cargo.toml` and `Cargo.lock`, plus one
  probe crate.
- `cargo tree -e normal,build` was run for `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu`.
  "New crates" means crates not already in rust-build's graph. "New dups" means crates that would
  break `multiple-versions = "deny"`.
- Build cost is a cold `cargo build -j4 -p probe` (dev profile, `CARGO_INCREMENTAL=0`, no sccache)
  on an M3 Max. CPU is user + sys time.
  - Baseline is tokio + rustls + tokio-rustls (ring): **26.6 s CPU / ~12 s wall**.
  - Each candidate was run once. Expect about ±3 s of noise.
- Versions, features and release dates come from `cargo info` and the crates.io API. Repo activity
  comes from the GitHub API, read 2026-11-21.

One finding applies to every Postgres driver, so it comes first:

> **Every maintained Postgres driver adds the same two duplicate families.**
>
> - tokio-postgres ≥ 0.7.17 / postgres-protocol ≥ 0.6.11 and sqlx 0.9 use `rand 0.10` and RustCrypto
>   `sha2 0.11`/`hmac 0.13`/`md-5 0.11` (`digest 0.11`).
> - axum 0.8.9 uses `sha1 0.10` and tokio-tungstenite 0.29, which bring `rand 0.9` and `digest 0.10`.
> - The result is duplicates of `digest`, `block-buffer`, `crypto-common`, `cpufeatures`, `rand` and
>   `rand_core`.
>
> Pinning `tokio-postgres =0.7.16` / `postgres-protocol =0.6.10` removes the duplicates; measured,
> the result has zero new duplicates. But those versions are hit by **RUSTSEC-2026-0178/0179/0180**,
> which are DoS bugs fixed in 0.7.18 / 0.6.12, and `cargo deny` fails on them. So we accept the
> duplicates with `skip` entries and remove them when axum moves to tungstenite ≥ 0.30, which is
> already on rand 0.10 / sha1 0.11. This agrees with `docs/research/job-queue.md`.

## 1. Postgres driver

| | **tokio-postgres 0.7.18 + deadpool-postgres 0.14.2** | + tokio-postgres-rustls 0.14 | sqlx 0.9.1, runtime API only | sqlx 0.9.1 + `macros` | diesel 2.4.0 + diesel-async 0.10.0 | sea-orm 2.0.4 | cornucopia 1.0.1 (codegen) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Latest release | 2026-06-12 / 2026-08-26 | 2026-05-21 | 2026-11-18 (3 days old) | same | 2026-11-13 | 2026-09-27 | 2026-08-14 (clorinde merged back, now archived) |
| New crates | 38 (bb8-postgres: 37) | 45 | 75 (url, idna, ICU4X, crossbeam, hashlink…) | 77 | 51 | 101 | runtime ≈ tokio-postgres + `postgres-types/derive` (+`postgres`, `futures`) |
| New proc-macros | none (async-trait only) | + der_derive | displaydoc, yoke/zerofrom/zerovec-derive (ICU) | + sqlx-macros | diesel_derives, darling | 14 | postgres-derive |
| New dups beyond the common pair | none | const-oid 0.9↔0.10 (x509-cert) | hashbrown 0.16↔0.17 | same | none | hashbrown, heck | none |
| Cold build CPU over baseline | **+9 s** | +12 s | +23 s | **+52 s**: sqlx-core, sqlx-postgres, tokio and rustls compile twice (host copy for the proc-macro, verified in `target/`) | +22 s | +44 s | ≈ tokio-postgres |
| rustls + ring | **bring your own `MakeTlsConnect`**: reuses `rb_tls::client_config()` exactly | `ring` feature; still builds from your `ClientConfig` | `tls-rustls-ring-webpki`; sqlx builds its own config (root cert / client cert by path or bytes) | same | user-supplied TLS (custom setup callback) | via sqlx | inherits tokio-postgres |
| Pooling | deadpool (statement cache `prepare_cached`, `RecyclingMethod::Fast`) or bb8 | — | built-in `PgPool` | — | deadpool / bb8 / mobc | via sqlx | deadpool-postgres |
| Runtime perf | fastest async class. A connection pipelines concurrent queries. | — | slower than tokio-postgres/diesel in diesel's benchmark suite | — | ≈ tokio-postgres (it's built on it) | sqlx + ORM overhead | hand-written tokio-postgres speed |
| Test story | real-DB tests in CI with a per-test `CREATE DATABASE` helper. Fakes go behind traits. | — | `#[sqlx::test]` gives an ephemeral DB per test, but needs `macros` | — | `begin_test_transaction` | `MockDatabase` | as tokio-postgres |
| Closeness to sqlc | hand-written fns mirror sqlc output 1:1 (2 queries today) | — | `query_file_as!` checks `.sql` files at compile time. Needs `DATABASE_URL` or the `.sqlx/` offline cache. | — | DSL, far | ORM, far | **closest**: annotated `.sql` files → generated crate. Needs a live PG (docker `schema`, or `fresh`/`live`) at codegen. |

### sqlx's offline-mode cost in CI

- `query!`/`query_file!` connect to `DATABASE_URL` at compile time. Offline builds need a committed
  `.sqlx/` (written by `cargo sqlx prepare --workspace`) plus `SQLX_OFFLINE=true`.
- CI also needs a job that runs `cargo sqlx prepare --check` against a live, migrated Postgres. That
  job needs sqlx-cli, whose default features pull native-tls/OpenSSL, so install it with
  `--no-default-features --features rustls,postgres`.
- Every query edit means re-running `prepare`.
- On top of that, the `macros` feature doubles the sqlx/tokio/rustls build (measured above). Our
  `[profile.dev.build-override]` makes host artifacts differ from target ones, so they can't be
  shared.

## 2. Valkey / Redis client

| | **redis 1.8.2** (`tokio-rustls-comp`, `connection-manager`) | redis 1.8.2 + `cluster-async` | redis 2.0.0-rc.3 | fred 10.1.0 (`i-keys`, `enable-rustls-ring`) | deadpool-redis 0.23.1 / bb8-redis 0.26.0 | valkey-glide 0.10.0 / glide-core 0.3.0 |
| --- | --- | --- | --- | --- | --- | --- |
| Maintenance | 2026-11-14, 38.9 M recent downloads. Keyword `valkey`. | same | pre-release, same day | **last release 2025-02-27, no pushes since** | 2026-08-26 / 2025-12-09 | 2026-11-12. glide-core says it "isn't intended to be used directly". |
| New crates | 44 (url → idna → ICU4X ≈ 20, combine, backon, arc-swap, rustls-native-certs) | 49 | — | 62 | 37 (no TLS) | 274 in its tree |
| New proc-macros | displaydoc, rustversion, ICU derives | same | — | 9 (fred-macros, futures-macro, async-trait…) | ICU derives | 19 |
| New dups | **none** | rand 0.9↔0.10 (dedups once axum moves) | moves to base64 0.23 / rand 0.10 | rand 0.8 family, socket2 0.5 | none | 14 duplicates, **aws-lc-rs + aws-lc-sys (banned)** |
| Cold build CPU over baseline | **+14 s** | — | — | +26 s | ≈ redis | — |
| rustls + ring | rustls `default-features=false`. Builds its own `ClientConfig::builder()` from the process default provider, which is ring because only ring is compiled. Custom CA and mTLS go through `TlsCertificates` (PEM). For full `rb_tls` config reuse, wrap our own tokio-rustls stream in `MultiplexedConnection::new_with_config`, which loses `ConnectionManager`'s auto-reconnect. | same | same | `enable-rustls-ring` (plain `enable-rustls` = aws-lc) | via redis | aws-lc |
| Pooling | none needed: `MultiplexedConnection`/`ConnectionManager` are `Clone` and pipeline | same | same | built-in | pool wrapper; only useful for blocking commands | — |

## 3. Migrations

| | **Own golang-migrate-compatible runner** (tokio-postgres) | sqlx migrate 0.9.1 | refinery 0.10.0 | diesel_migrations 2.4.0 |
| --- | --- | --- | --- | --- |
| Keeps `sql/migrations/00000N_name.up.sql` + `.down.sql` | **yes, unchanged** | **yes**: `<VERSION>_<DESC>.up.sql/.down.sql`, version parsed as i64 (`000001` → 1) | no: `V1__name.sql`, and **no down migrations** by design (Flyway philosophy) | no: one directory per migration, `{version}_{name}/up.sql, down.sql` |
| State table | **same `schema_migrations(version bigint, dirty bool)`** and the same advisory-lock id as golang-migrate, so the Go and Rust tools can share one DB during the port | `_sqlx_migrations` (checksummed, SHA-384). Existing Go DBs need a backfill. | `refinery_schema_history` | `__diesel_schema_migrations` |
| Embed in binary | `include_str!` list + a unit test asserting it matches the directory | `migrate!()` (needs `macros`) or `Migrator::new(path)` at runtime | `embed_migrations!` proc-macro | `embed_migrations!` proc-macro |
| Down / status API | we write `status`, `up_all`, `up_one`, `down_one`, `force(v)` (≈200 lines) | `run`, `run_to`, `undo(target)`, `list_applied_migrations` | up only | `run_pending`, `revert_last`, `applied_migrations` |
| Driver coupling | tokio-postgres | sqlx only | tokio-postgres works, but its TLS feature is native-tls; pass a connected `Client` instead | sync diesel (libpq via `postgres`) or diesel-async's `AsyncConnectionWrapper` |
| Cost | 0 new crates (`crc32fast` for the lock id is already in `Cargo.lock`) | part of sqlx | +16 s CPU over tokio-postgres (regex, walkdir, url/ICU, refinery-macros) | diesel + toml + 3 proc-macros |

golang-migrate's semantics, to reproduce exactly:

- **Before each file:** write `version = N, dirty = true`.
- **Run the file:** run the whole file with `batch_execute`, using the simple-query protocol. That
  gives the same implicit-transaction behaviour as pgx `Exec`.
- **After each file:** write `dirty = false`.
- **Dirty databases:** refuse to run while dirty until `force`.
- **Lock:** take the lock with `pg_advisory_lock(id)`.
  - `id = crc32_ieee("public\0schema_migrations\0<dbname>") * 1486364155`, computed as wrapping
    `u32`.
  - This formula comes from `database/util.go` and the `pgx/v5` driver.

## 4. Recommendation

**Use tokio-postgres 0.7.18 + deadpool-postgres 0.14.2 with our own rustls/ring `MakeTlsConnect`,
redis 1.8.2 for Valkey, and an in-house golang-migrate-compatible migrator.** Write the two queries
by hand for now, mirroring sqlc's output. Adopt cornucopia 1.x when the query count grows (it
supports `use-workspace-deps = true`). Put everything behind off-by-default features.

Why:

- **Smallest graph.** Postgres + Valkey together cost **+24 to 26 s CPU** and 82 new crates,
  versus +31 s for sqlx-runtime + redis. Adding sqlx `macros` makes it about +60 s.
- **No new proc-macros** on the Postgres side.
- **Exact `ClientConfig` reuse** for Postgres.
- **The Go and Rust migrators are interchangeable** on the same database during the port.
- **Same seams as today:** `UserStore`, `Migrator`, `Cache`.

### `[workspace.dependencies]`

```toml
# Postgres (feature "postgres"). Default features off: no NoTls-only surprises, no with-* types yet.
# Add "with-serde_json-1" / a time crate feature when the otel/investigations tables are ported.
tokio-postgres = { version = "0.7.18", default-features = false, features = ["runtime"] }
deadpool-postgres = { version = "0.14.2", default-features = false, features = ["rt_tokio_1"] }
# golang-migrate advisory-lock id. Already in Cargo.lock (flate2), so no new crate.
crc32fast = "1"
# Valkey (feature "valkey"). No cluster: cluster-async adds a rand 0.10 duplicate until axum moves.
redis = { version = "1.8.2", default-features = false, features = ["tokio-rustls-comp", "connection-manager"] }
```

Do **not** add `tokio-postgres-rustls`. It pulls x509-cert/der/spki and a second `const-oid`. The
connector is about 70 lines in a new `rb-db` crate:

- `MakeTlsConnect<Socket>` wraps a `tokio_rustls::TlsConnector` built from
  `rb_tls::client_config(ClientOptions { alpn: vec![b"postgresql".to_vec()], .. })`.
  - Don't reuse the HTTP ALPN list. PostgreSQL 17+ checks ALPN.
- `TlsConnect::connect` returns a boxed `Send` future.
- `TlsStream::channel_binding()` returns `ChannelBinding::none()`.
  - SCRAM-SHA-256 still works; you lose only `-PLUS`.
  - Add `tls-server-end-point` later with `ring::digest` if needed.

### Crate layout and features

```toml
# crates/rb-db/Cargo.toml: one crate, two optional backends, default build unchanged
[features]
postgres = ["dep:tokio-postgres", "dep:deadpool-postgres", "dep:crc32fast"]
valkey = ["dep:redis"]

[dependencies]
rb-core.workspace = true
rb-tls.workspace = true
tokio.workspace = true
tokio-rustls.workspace = true
rustls.workspace = true
tokio-postgres = { workspace = true, optional = true }
deadpool-postgres = { workspace = true, optional = true }
crc32fast = { workspace = true, optional = true }
redis = { workspace = true, optional = true }
```

`rb-server` and `apps/rb` forward the features: `postgres = ["rb-db/postgres"]`,
`valkey = ["rb-db/valkey"]`. Put `sql/migrations/` and `sql/queries/` at the repo root, copied
verbatim from golang-build.

### Fakes and tests

- **`UserStore`** (rb-server) keeps `MemoryStore`. Add `PgUserStore(deadpool_postgres::Pool)` under
  `postgres`. Mirror the sqlc pair:
  - `list` maps to `SELECT id, name FROM users WHERE id > $1 ORDER BY id LIMIT $2`.
  - `create` maps to `INSERT … RETURNING id, name`.
  - Prepare both with `prepare_cached`.
- **Cache.** Add an async `Cache` trait (`get`, `set(ttl)`, `invalidate(ns)`) in rb-core, std-only
  with `BoxFut`.
  - `MemoryCache` wraps today's `kv::Memory<String>`.
  - `ValkeyCache` uses a `ConnectionManager` with `GET` / `SET EX`.
  - Replace `sweep_prefix("users:")` with a generation key (`INCR users:gen`, with the generation
    in every cache key) rather than `SCAN` + `DEL`.
- **`Migrator` trait.** Methods: `status`, `up_all`, `up_one`, `down_one`, `force`. Two impls:
  - `PgMigrator` (embedded files).
  - `FakeMigrator`: an in-memory version + dirty flag, a port of the Go fake.
  - `rb migrate --fake` uses the fake, as in Go.
- **Real-backend tests.**
  - Location: `apps/rb/tests/e2e.rs` (one binary, per AGENTS.md), `#[cfg(feature = "postgres")]`.
  - Gating: skipped unless `RB_TEST_DATABASE_URL` / `RB_TEST_VALKEY_URL` is set, so `cargo t`
    stays offline.
  - Isolation: a small `TestDb` helper does `CREATE DATABASE rb_test_<id>`, runs the embedded
    migrations (up, then down, then up, to exercise the down files) and drops the database
    afterwards. This replaces `#[sqlx::test]`.
  - CI: one `integration` job with GitHub Actions `services:` for `postgres:17` and
    `valkey/valkey:8`.
- **Avoid testcontainers.** testcontainers-modules 0.15 adds 98 crates (bollard, tonic, prost) and
  resolves testcontainers 0.27.3, not 0.28.

### `deny.toml` changes

```toml
# under [bans] skip; remove all six when axum moves to tokio-tungstenite >= 0.30
{ crate = "digest@0.10", reason = "axum 0.8 / tungstenite 0.29 use sha1 0.10; postgres-protocol 0.6.12 uses sha2/hmac/md-5 on digest 0.11" },
{ crate = "block-buffer@0.10", reason = "same as digest@0.10" },
{ crate = "crypto-common@0.1", reason = "same as digest@0.10" },
{ crate = "cpufeatures@0.2", reason = "same as digest@0.10" },
{ crate = "rand@0.9", reason = "tungstenite 0.29 (axum 0.8) uses 0.9; tokio-postgres 0.7.18 uses 0.10" },
{ crate = "rand_core@0.9", reason = "same as rand@0.9" },

# under [licenses]: redis has a mandatory dependency on xxhash-rust, which is BSL-1.0 only
exceptions = [{ crate = "xxhash-rust", allow = ["BSL-1.0"] }]
```

`[graph] all-features = false` means cargo-deny **never sees feature-gated backends**. Add a CI step
that runs `cargo deny --all-features check`, or set `all-features = true`, so the new skips and the
license exception are actually enforced.

## Risks

1. **The duplicate-version skips are real policy debt.** Six small crates compile twice whenever
   `postgres` is on, until axum 0.9 / tungstenite 0.30. Don't "fix" this by pinning tokio-postgres
   below 0.7.18: that brings back three RustSec DoS advisories.
2. **redis-rs is the heaviest piece.**
   - Its mandatory `url` brings ICU4X (≈20 crates, 4 proc-macros).
   - It builds its own rustls config, from the process default provider.
     - Call `rustls::crypto::ring::default_provider().install_default()` at startup, so that a
       future second provider can't make `ClientConfig::builder()` panic.
   - redis 2.0 is at rc.3 (2026-11-14). Expect API churn soon. Keep every call inside `ValkeyCache`.
3. **We own the migrator and the queries.**
   - There's no compile-time SQL checking. The e2e job against real Postgres is the safety net.
   - Cornucopia is the upgrade path. Its generated crate also depends on `postgres` + `futures`;
     re-measure when adopting it.
   - Migrator correctness (dirty handling, lock id) needs a test against a DB that golang-migrate
     has already migrated.
4. **ALPN and channel binding on the Postgres TLS connector.**
   - Always set the `postgresql` ALPN (or none).
   - `ChannelBinding::none()` means `channel_binding=require` URLs will fail.

## Sources

- Workspace constraints: `Cargo.toml`, `deny.toml`, `AGENTS.md`; golang-build `internal/db`,
  `internal/cache`, `cmd/migrate`, `sql/`, `sqlc.yaml`.
- tokio-postgres: <https://crates.io/crates/tokio-postgres>, changelog
  <https://github.com/rust-postgres/rust-postgres/blob/master/tokio-postgres/CHANGELOG.md>,
  <https://github.com/rust-postgres/rust-postgres/blob/master/postgres-protocol/CHANGELOG.md>
- RustSec: <https://rustsec.org/advisories/RUSTSEC-2026-0178.html>,
  <https://rustsec.org/advisories/RUSTSEC-2026-0179.html>,
  <https://rustsec.org/advisories/RUSTSEC-2026-0180.html>
- deadpool-postgres: <https://crates.io/crates/deadpool-postgres>, <https://github.com/deadpool-rs/deadpool>;
  bb8: <https://github.com/djc/bb8>
- tokio-postgres-rustls: <https://github.com/jbg/tokio-postgres-rustls>
- sqlx (repo now at transact-rs): <https://github.com/transact-rs/sqlx>, <https://crates.io/crates/sqlx>,
  sqlx-cli offline mode: <https://github.com/transact-rs/sqlx/blob/main/sqlx-cli/README.md>,
  migrator: <https://docs.rs/sqlx/0.9.1/sqlx/migrate/struct.Migrator.html>
- diesel / diesel-async / diesel_migrations: <https://crates.io/crates/diesel>,
  <https://github.com/diesel-rs/diesel_async>, <https://docs.rs/diesel_migrations/2.4.0>;
  benchmarks: <https://github.com/diesel-rs/metrics>
- sea-orm: <https://github.com/SeaQL/sea-orm>
- cornucopia 1.0 (clorinde merged back): <https://github.com/cornucopia-rs/cornucopia>,
  <https://cornucopia-rs.github.io/cornucopia/configuration.html>,
  <https://github.com/halcyonnouveau/clorinde> (archived)
- redis-rs: <https://github.com/redis-rs/redis-rs>, <https://crates.io/crates/redis>,
  <https://docs.rs/redis/1.8.2/redis/aio/struct.MultiplexedConnection.html>,
  <https://docs.rs/redis/1.8.2/redis/struct.TlsCertificates.html>
- fred: <https://github.com/aembke/fred.rs>; deadpool-redis: <https://crates.io/crates/deadpool-redis>;
  bb8-redis: <https://crates.io/crates/bb8-redis>
- Valkey GLIDE: <https://github.com/valkey-io/valkey-glide>, <https://crates.io/crates/valkey-glide>,
  <https://crates.io/crates/glide-core>
- refinery: <https://github.com/rust-db/refinery> (README "Rollback" section)
- golang-migrate lock id and table: <https://github.com/golang-migrate/migrate/blob/master/database/util.go>,
  <https://github.com/golang-migrate/migrate/blob/master/database/pgx/v5/pgx.go>
- tungstenite 0.30 deps (rand 0.10, sha1 0.11): <https://crates.io/crates/tungstenite/0.30.0>;
  axum 0.8.9: <https://crates.io/crates/axum/0.8.9>
- testcontainers: <https://github.com/testcontainers/testcontainers-rs>,
  <https://crates.io/crates/testcontainers-modules>
- Related: `docs/research/job-queue.md` (reaches the same driver choice independently)
