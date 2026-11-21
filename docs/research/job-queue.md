# Background job queue for rust-build

Research date: 2026-11-21. Toolchain: Rust 1.99.1, edition 2024, tokio. Status: recommendation, not
yet implemented.

## What we are replacing

golang-build runs [River](https://riverqueue.com) v0.35.1 (`riverqueue/river` +
`riverdriver/riverpgxv5`) on a pgx pool (`internal/jobs/client.go`, `cmd/server/main.go`). It uses
these River features:

| River feature (Go) | Where | Needed in Rust |
| --- | --- | --- |
| Postgres-backed queue, `rivermigrate` at boot | `jobs.Migrate` | yes |
| Two queues with per-process caps: `default` (10), `interactive` (2) | `jobs.NewClient` | yes |
| `JobTimeout` 2 min, `RescueStuckJobsAfter` 5 min | `jobs.NewClient` | yes |
| Graceful `Stop` for 8 s, then `StopAndCancel` | `jobs.Stop` (shutdown coordinator) | yes |
| Unique jobs `ByArgs` + `ByPeriod` (30 s to 10 min) | `jobs`, `tracing`, `profiling`, `investigations` | yes |
| Periodic job (`PeriodicInterval(1m)`, `RunOnStart`) | `tracing.SchedulePeriodicBaselines` | yes |
| Per-worker `Timeout` | `PingWorker` | yes |
| Retries with backoff (River's default `attempt^4` s) | implicit | yes |
| Jobs enqueued from other jobs (`Client` passed to workers) | `investigations`, `tracing` sink | yes |
| Transactional enqueue (`InsertTx`) | not used today, but it's River's main selling point | strongly wanted |
| River UI | not deployed | nice to have |

## Constraints from this repo

From `Cargo.toml`, `deny.toml`, `AGENTS.md`:

- TLS only through rustls + ring. `aws-lc-rs`, `aws-lc-sys`, `openssl-sys` and `native-tls` are
  banned.
- `multiple-versions = "deny"`. Every duplicate needs a `skip` entry with a reason.
- License allow-list: Apache-2.0, BSD-2/3-Clause, CDLA-Permissive-2.0, ISC, MIT, Unicode-3.0, Zlib.
  MPL-2.0 and the PostgreSQL license are **not** on it.
- Fast compile is a top priority: default features off, one feature set per dependency, no doctests.
- Time goes through `rb_core::clock::Clock` and retry sleeps through `rb_core::retry::Sleeper`, so
  tests never really wait.

## Method

- Versions, dates and download counts come from the crates.io API (`/api/v1/crates/<name>`) and
  `cargo info` (1.99.1). GitHub stars and activity come from `gh api repos/<owner>/<repo>`. All
  were fetched on 2026-11-21.
- **Dependency probe.** I copied the root workspace (without `desktop/`) to a scratch directory,
  kept its `Cargo.lock`, and added a probe crate that depends on each candidate with
  default-features off and TLS on rustls/ring wherever the crate allows it. For each probe I ran
  `cargo tree --target all -e normal,build` over the same five targets `deny.toml` checks, and
  counted unique crates, duplicates and banned crates. The baseline workspace has 158 crates, and
  its only duplicates are the five already in `deny.toml`'s `skip` list.
- **Build probe.** A clean `cargo build -p probe` (dev profile) of `rb-server` plus the candidate,
  timed with `/usr/bin/time`. The machine was heavily loaded by other builds while these ran (load
  average 60 to 100 on 16 cores). Treat wall time as noisy. Unit count and CPU time are more
  reliable.

## Dependency weight (measured)

"New duplicate families" leaves out the five that are already skipped (base64, getrandom, r-efi,
syn, windows-sys) and the Windows/wasi target families.

| Probe | Crates (baseline 158) | New duplicate families under `deny.toml` | Banned crates |
| --- | --- | --- | --- |
| **sqlx 0.9.1** alone (`postgres`, `runtime-tokio`, `tls-rustls-ring-webpki`, `json`) | 236 | digest/block-buffer/crypto-common/cpufeatures 0.10↔0.11, rand/rand_core 0.9↔0.10, hashbrown 0.16↔0.17 | none |
| **tokio-postgres 0.7.18 + deadpool-postgres 0.14.2** | 215 (+tokio-postgres-rustls 0.13; 208 without it) | digest family, rand family; tokio-postgres-rustls adds const-oid 0.9↔0.10 (via x509-cert) | none |
| **riverqueue 0.6.0** (+ sqlx as above) | 252 | same as sqlx | none |
| riverqueue + riverqueue-test 0.6.0 | 253 | same as sqlx | none |
| riverqueue + `sqlite` feature | 259 | same as sqlx (+ libsqlite3-sys C build) | none |
| apalis 1.2.0 + apalis-postgres 1.1.1 + apalis-cron 1.1.0 | 285 | sqlx's, plus phf 0.11↔0.12 and rand 0.8 | none |
| apalis 1.2.0 + apalis-redis 1.1.1 | 220 | cpufeatures, rand family | none |
| graphile_worker 0.14.2 (default: sqlx + rustls) | 304 | same as sqlx | none |
| graphile_worker 0.14.2 (`driver-tokio-postgres`) | 277 | digest family, rand family | none, but no TLS: you supply it |
| awa 0.6.18 | 282 | **a second sqlx (0.8.6)**, rand 0.8, webpki-roots 0.26, hashbrown 0.15, … | none |
| underway 0.2.0 | 263 | **sqlx 0.8.6**, rand 0.8, webpki-roots 0.26, hashbrown 0.15 | none |
| pgmq 0.33.7 | 262 | **sqlx 0.8.6**, rand 0.8, thiserror 1 | none |
| sqlxmq 0.6.0 (`runtime-tokio-rustls`) | 264 | **sqlx 0.8.6**, syn 1, rand 0.8, webpki-roots 0.26 | none |
| fang 0.11.0 (`asynk-postgres`) | 285 | **sqlx 0.8.6**, sha2 0.10↔0.11, rand 0.8 | none |
| backie 0.9.0 | 231 | thiserror 1, digest family | none |
| rusty-sidekiq 0.15.2 | 237 | **none** | none |
| redis 1.8.2 alone (`tokio-comp`, `tokio-rustls-comp`, `connection-manager`) | 204 | **none** | none |
| faktory 0.13.1 | 237 | rand 0.8, thiserror 1, synstructure | none |
| effectum 0.7.0 (SQLite) | 241 | rand 0.8, thiserror 1, hashbrown 0.14, bundled libsqlite3 | none |
| aide-de-camp 0.2.0 (SQLite) | 206 | bitflags 1, rand 0.8, thiserror 1 | none |

Three findings matter more than any single row:

1. **Adding any current Postgres driver introduces the same two duplicate families.** sqlx 0.9 and
   tokio-postgres 0.7.18 both use `sha2 0.11`/`hmac 0.13`/`md-5 0.11` (digest 0.11) and
   `rand 0.10`. axum 0.8.9 and tungstenite 0.29 still use `sha1 0.10` (digest 0.10) and `rand 0.9`.
   You can't avoid this by choosing a queue. It needs two `deny.toml` `skip` entries, with "axum's
   tungstenite 0.29 is on digest 0.10 / rand 0.9" as the reason, until axum moves. sqlx also adds
   `hashbrown 0.16` (through hashlink), which tokio-postgres does not.
2. **Every crate still on sqlx 0.8 is disqualified if we choose sqlx 0.9**, because it compiles a
   second sqlx-core/sqlx-postgres. That covers awa, underway, pgmq, sqlxmq and fang. If we choose
   tokio-postgres instead, they would still pull in all of sqlx 0.8.
3. No candidate pulls aws-lc or OpenSSL once its features are set as above. Watch the defaults,
   though: sqlxmq defaults to `runtime-tokio-native-tls`, `tokio-postgres-rustls` has an
   `aws-lc-rs` feature, and sqlx's `tls-rustls-aws-lc-rs*` features exist. Always enable sqlx's
   TLS as `tls-rustls-ring-webpki` (or `tls-rustls-ring-native-roots`).

### Clean build cost (measured, noisy)

`cargo build -p probe` with `rb-server` plus the candidate, from an empty target directory:

CPU time is `user` from `/usr/bin/time`. Two runs of the key rows agreed within about 3 s CPU; wall
time varied with machine load. "Units" counts cargo fingerprints: build scripts, proc-macros and
libraries, including host-side duplicates.

| Probe (on top of `rb-server`) | Units | CPU user s (run 1 / run 2) | Δ CPU vs baseline | Wall s (noisy) |
| --- | --- | --- | --- | --- |
| baseline: `rb-server` only | 154 | 51.3 / 54.4 | — | 11–16 |
| tokio-postgres + deadpool-postgres | 194 | 60.0 / 58.2 | **≈ +6** | 15–28 |
| … + tokio-postgres-rustls | 201 | 61.8 | ≈ +9 | 15 |
| sqlx 0.9 (postgres, rustls-ring, json) | 239 | 72.2 / 72.1 | ≈ +19 | 21 |
| **riverqueue 0.6 + sqlx** | 289 | 106.4 / 106.6 | **≈ +53** | 27–32 |
| graphile_worker (tokio-postgres) | 264 | 74.5 / 72.9 | ≈ +21 | 15–17 |
| graphile_worker (sqlx) | 330 | 111.9 | ≈ +59 | 32 |
| apalis + apalis-postgres + apalis-cron | 314 | 114.5 | ≈ +62 | 30 |
| awa 0.6 | 295 | 114.0 | ≈ +61 | 29 |
| underway 0.2 | 271 | 102.3 | ≈ +49 | 29 |
| redis 1.8 alone | 204 | 67.5 | ≈ +15 | 23 |
| apalis + apalis-redis | 214 | 67.8 | ≈ +15 | 15 |
| rusty-sidekiq | 233 | 75.8 | ≈ +23 | 17 |

Every sqlx-based row pays for a second compile of sqlx-core, sqlx-postgres, tokio and rustls on
the proc-macro host side whenever something enables sqlx's `derive` or `macros` feature. That covers
riverqueue (`derive`), apalis-postgres (`migrate` turns on `macros`) and graphile_worker's sqlx
driver. Plain sqlx without those features (row 4) doesn't. These are clean-build costs. CI caches
dependencies by lockfile, and incremental edit-check-test loops only rebuild our own crates.

## Feature comparison

Legend: ✅ built in, 🟡 partial or needs glue, ❌ missing.

| | riverqueue 0.6.0 | apalis 1.2 + apalis-postgres 1.1.1 | graphile_worker 0.14.2 | underway 0.2.0 | awa 0.6.18 | sqlxmq 0.6.0 | fang 0.11.0 | rusty-sidekiq 0.15.2 | Own minimal queue |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Latest release | 2026-11-05 (first 0.1.0: 2026-10-07) | 2026-11-14 / 2026-11-18 | 2026-11-10 | 2025-07-16 | 2026-11-18 (first: 2026-03-18) | 2025-05-25 | 2026-07-02 | 2026-09-28 | — |
| Repo activity (stars, last push) | riverqueue/river: 5.9k, 2026-11-14 (shared with River Go) | 1.5k, 2026-11-20 | 89, 2026-11-21 | 176, 2026-11-19 | 37, 2026-11-21, 102 open issues | 168, 2025-05-25 | 722, 2026-07-02 | 168, 2026-09-28 | us |
| License | **MPL-2.0** (not on allow-list) | MIT (apalis-postgres declares only `license-file`, so cargo-deny needs a `clarify`) | MIT | MIT/Apache-2.0 | MIT/Apache-2.0 | MIT/Apache-2.0 | MIT | MIT | ours |
| Backend / driver it forces | sqlx 0.9 (Postgres or SQLite) | sqlx 0.9; Redis via `redis` 1.8 | sqlx 0.9 **or** tokio-postgres | sqlx 0.8 ❌ | sqlx 0.8 ❌ (+ opt. tokio-postgres) | sqlx 0.8 ❌ | sqlx 0.8 ❌ (+ diesel for blocking) | Redis/Valkey | whichever we choose |
| Transactional enqueue | ✅ `.tx(&mut tx)` on every insert/query; notify on commit | 🟡 `queries::push_tasks(&mut *tx, …)` (low-level fn) | ✅ `WorkerUtils::with_executor(tx)` | ✅ | ✅ | ✅ | ✅ | ❌ (Redis) | ✅ |
| Retries / backoff | ✅ durable reschedule, `RetryPolicy` trait, snooze, discard | 🟡 tower `RetryLayer` retries in the worker (the slot is held during backoff); attempts persisted | ✅ exponential, `max_attempts` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ via `rb_core::retry::Policy` |
| Scheduled / periodic | ✅ `scheduled_at`; periodic with cron (5-field, `@every`), `run_on_start`, leader-elected | ✅ `run_after`; cron via apalis-cron (cron + chrono) | ✅ `run_at`; crontab | ✅ cron (jiff-cron) | ✅ | 🟡 delay only | ✅ | ✅ cron | ✅ interval schedules; cron optional |
| Unique / dedup | ✅ by args (or selected fields), period, queue, state; same hashes as Go | 🟡 `idempotency_key` | 🟡 `job_key` (replace/preserve) | 🟡 `concurrency_key` only | ✅ windowed | ❌ | 🟡 `uniq` | ✅ `unique_for` | ✅ unique key + partial index |
| Concurrency limits | ✅ per-queue `QueueConfig::new(n)`, runtime add/remove/pause | ✅ tower limit / worker concurrency | ✅ global concurrency, serial named queues | ✅ | ✅ weighted, rate limits | ✅ | ✅ | ✅ | ✅ per-queue semaphore |
| Graceful shutdown | ✅ soft stop, `stop_and_cancel`, job timeouts, stuck-job rescuer | ✅ monitor shutdown, orphan re-enqueue | ✅ + heartbeat recovery | ✅ | ✅ heartbeat rescue | 🟡 | 🟡 | ✅ | ✅ |
| Observability | ✅ `tracing`, event stream, `Hook::metric_emit`, insert/work middleware, **River UI** reads the same schema | ✅ tracing, prometheus/otel features, apalis-board UI | ✅ tracing spans, lifecycle hooks, otel features, admin UI crate | 🟡 tracing | ✅ otel (always compiled) | 🟡 | 🟡 | 🟡 | 🟡 tracing spans + our stats hooks |
| Testing without a DB | ✅ `riverqueue-test`: `work_once`, `TestJobBuilder`; optional SQLite in-memory backend | ✅ in-memory backend in apalis-core | ❌ needs Postgres | ❌ | ❌ | ❌ | ❌ | ❌ needs Redis | ✅ in-memory store on `Clock` |
| Valkey backend | ❌ | ✅ apalis-redis | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ (only backend) | not planned |
| Fit with rust-build | runner-up: best feature fit; MPL-2.0, 6 weeks old, ≈ +53 s CPU clean build, no Clock injection | heavy (≈ +62 s); Postgres retries aren't durable | fallback on tokio-postgres (≈ +21 s); no DB-less tests | ❌ sqlx 0.8 | ❌ sqlx 0.8, young | ❌ stale, sqlx 0.8 | ❌ sqlx 0.8 + diesel | Valkey-only alternative | **recommended**: ≈ +6 s, Clock-tested; we own ~1.5k lines |

Not shortlisted:

- **backie 0.9.0**: GitHub repo archived, last release 2023-07-12, uses diesel-async.
- **faktory 0.13.1**: needs a separate Faktory server process, and adds rand 0.8 and thiserror 1.
- **effectum 0.7.0**: SQLite only, last release 2024-07-23, bundles libsqlite3 (a C build).
- **aide-de-camp 0.2.0**: SQLite only, last release 2022-12-18, GitHub repo gone (404).
- **pgmq 0.33.7** (0.34 is alpha): a message-queue primitive (visibility timeout,
  send/read/archive). It has no worker runtime, retries policy, cron or uniqueness, uses the
  PostgreSQL license (not on our allow-list), and the stable release is on sqlx 0.8. Its
  tokio-postgres support is marked experimental.
- **pgqueue 0.3.4**: first published 2026-09-28, about 500 downloads, 0 stars. Too new to depend on.
- **river-rs 0.3.3**: not River. It's an unrelated hobby crate, last released 2025-04.

## Notable finding: an official River client for Rust (riverqueue)

River's maintainers now ship an official Rust client in the River monorepo
(`riverqueue/river/rust`, crates `riverqueue`, `riverqueue-macros`, `riverqueue-migrate`,
`riverqueue-test`, `riverqueue-cli`). Its README says it shares River's database schema and job
protocol with the clients in other languages, so Rust and Go services can insert and work jobs in
one database. It maps one-to-one onto everything golang-build uses:

| Go (`internal/jobs`) | riverqueue 0.6 |
| --- | --- |
| `river.NewClient(riverpgxv5.New(pool), &river.Config{…})` | `Client::builder(pg_pool)` |
| `Queues: {default: {MaxWorkers: 10}, interactive: {MaxWorkers: 2}}` | `.queue("default", QueueConfig::new(10)).queue("interactive", QueueConfig::new(2))` |
| `JobTimeout`, `RescueStuckJobsAfter` | `.job_timeout(..)`, `.maintenance(MaintenanceConfig::default().with_rescue_after(..))` |
| `Stop` then `StopAndCancel` | `RunHandle::stop` / `stop_and_cancel`, `.soft_stop_timeout(..)` |
| `UniqueOpts{ByArgs, ByPeriod}` | `#[river(kind = "ping", unique(by_args, by_period = "1m"))]` |
| `NewPeriodicJob(PeriodicInterval(1m), …, RunOnStart)` | `.periodic_job(PeriodicJob::with_options(IntervalSchedule::new(60s)?, …, PeriodicJobOpts::default().with_run_on_start(true)))` |
| `Worker.Timeout` | `impl Worker<A> { fn timeout(&self, job) -> WorkerTimeout }` |
| `rivermigrate` | `riverqueue_migrate::PostgresMigrator` or `riverqueue migrate-up` (CLI) |
| `InsertTx` | `client.insert(args).tx(&mut tx)` |
| River UI | the same `riverui` binary works, because the schema is the same |

Its dependency graph is clean. Beyond sqlx 0.9 it adds jiff (+ jiff-sqlx), tokio-util,
async-trait and its own macro/migrate crates: **+16 crates** and **no new duplicate families**. It
does not pick a TLS backend for sqlx, so we enable `tls-rustls-ring-webpki` ourselves.

Its *build* is not light, though. riverqueue turns on sqlx's `derive` feature, which pulls in the
`sqlx-macros` proc-macro and `sqlx-macros-core`. Under resolver 3, the host-side (proc-macro) copies
of `sqlx-core`, `sqlx-postgres`, `tokio` and `rustls` get a different feature set from the
target-side copies, so each **compiles twice**. cargo-deny doesn't catch this because the versions
are the same. Measured against plain sqlx: 239 → 289 units, and about +34 s CPU on a clean build
(see the build table). We can't switch this off from outside.

Caveats:

- **License: MPL-2.0.** MPL is file-level copyleft. Using it unmodified as a dependency places no
  obligations on our code, but `deny.toml` must allow it. Add `MPL-2.0` to `allow`, or better, add
  a crate-scoped `exceptions` entry for `riverqueue*`.
- **Age.** 0.1.0 shipped 2026-10-07; 0.6.0 is the ninth release in six weeks. Expect breaking API
  changes on minor bumps. Pin `=0.6.x` and bump on purpose.
- **Schema version.** riverqueue-migrate 0.6 ships River migrations 001–008. golang-build's River
  Go v0.35.1 stops at 006. Migration 007 arrived in River Go 0.40.0 and 008 in 0.48.0 (SQLite
  only), per River's CHANGELOG. The mixed-deployments guide also pins ByPeriod unique-key parity
  to River Go ≥ 0.48. So a mixed Go/Rust deployment on one database means upgrading River Go to
  ≥ 0.48 first. A fresh rust-build database has no such issue.
- **Time is not injectable.** Scheduling uses database time and tokio timers, not
  `rb_core::clock::Clock`. Unit tests use `riverqueue-test::work_once` (no DB, no clock). Behaviour
  across time (backoff, periodic, uniqueness windows) is covered by River's own tests, and we only
  smoke-test it against a real Postgres.
- It forces **sqlx**. If rust-build chooses tokio-postgres for the app, riverqueue would add sqlx
  on top. That costs about 20 to 30 extra crates and a second pool, and it breaks transactional
  enqueue because a sqlx transaction can't wrap a tokio-postgres write.

## Recommendation

**Build a small `rb-jobs` crate of our own (design below) on tokio-postgres 0.7 + deadpool-postgres
0.14, and make tokio-postgres the project's Postgres driver.** Use an in-memory store driven by
`rb_core::clock::Clock` for tests. Keep Valkey out of the job path.

```toml
# [workspace.dependencies]
tokio-postgres    = { version = "0.7", default-features = false, features = ["runtime", "with-serde_json-1"] }
deadpool-postgres = { version = "0.14", default-features = false, features = ["rt_tokio_1"] }
# TLS: a ~80-line MakeTlsConnect over the tokio-rustls/rb-tls config we already build,
# instead of tokio-postgres-rustls (which pulls x509-cert and a second const-oid).
```

Why this over riverqueue, which has the better feature list:

1. **Compile cost, our top priority.**
   - Added to `rb-server`, tokio-postgres + deadpool costs **+40 to 47 units** and roughly **+6 to 9 s
     CPU** on a clean build.
   - riverqueue on sqlx costs **+135 units** and roughly **+53 s CPU**. About +34 s of that is
     riverqueue itself, because sqlx's `derive` feature compiles sqlx/tokio/rustls a second time
     for the proc-macro host.
   - tokio-postgres has no proc-macros and adds no `hashbrown` duplicate.
2. **The feature surface we need is small.** It is the table at the top of this document:
   - two queues with worker caps,
   - a timeout and a rescuer,
   - retries with backoff,
   - interval-based periodic jobs (no cron),
   - ByArgs + ByPeriod uniqueness,
   - stop, then cancel.

   That is roughly 1,000 to 1,500 lines with tests. rb-core already supplies the backoff
   (`retry::Policy`), the clock, the stats and the event bus.
3. **It follows the repo's rules.**
   - Time goes through `Clock`, so backoff, schedules, uniqueness windows and rescue are tested
     without sleeping. riverqueue uses database time and tokio timers, so it can't do this.
   - No MPL-2.0 exception.
   - No dependency on a crate that is six weeks old and still breaking its API every release.
4. **Transactional enqueue works with the app's own writes**, because the queue and `UserStore` use
   the same tokio-postgres transaction type.

`deny.toml` changes (needed with **any** current Postgres driver, not because of the queue):

- `skip` entries for `digest@0.10`, `block-buffer@0.10`, `crypto-common@0.1`, `cpufeatures@0.2`,
  `rand@0.9` and `rand_core@0.9`.
- Reason: "axum 0.8 / tungstenite 0.29 still on sha1 0.10 / rand 0.9; tokio-postgres uses the
  RustCrypto 0.11 / rand 0.10 releases".
- Remove them when axum moves.

Wiring:

- `rb_server::serve`'s shutdown hook replicates `jobs.Stop`: stop fetching, wait 8 s, then cancel.
- The server's handlers depend on an `rb_jobs::Enqueue` trait. That trait is the same seam as
  `UserStore`.
- Real-Postgres tests live in `apps/rb/tests/e2e.rs`, gated on `DATABASE_URL`.

**Postgres driver implied: tokio-postgres 0.7.18 (+ deadpool-postgres 0.14.2), TLS via our rustls/ring config.**

### Runner-up: riverqueue 0.6 on sqlx 0.9

Choose this instead if any of these hold:

- the project picks **sqlx** for other reasons. riverqueue's marginal cost over sqlx is then only
  the +50 units / ~34 s CPU of the derive duplication.
- **River UI** or the job/queue management API is wanted.
- Rust has to share a database with the Go service.

```toml
sqlx = { version = "0.9", default-features = false, features = ["postgres", "runtime-tokio", "tls-rustls-ring-webpki", "json"] }
riverqueue = { version = "=0.6.0", default-features = false, features = ["postgres"] }
riverqueue-test = { version = "=0.6.0", default-features = false }   # dev-dependency
```

- Keep sqlx's `macros` and `migrate` features off; riverqueue migrates itself.
- Keep riverqueue's `sqlite` feature off: it is a libsqlite3 C build.
- `deny.toml` needs the same skips as above, plus `hashbrown@0.16` (from sqlx's hashlink).
- `deny.toml` also needs a crate-scoped MPL-2.0 exception for `riverqueue`, `riverqueue-macros`,
  `riverqueue-migrate` and `riverqueue-test`.
- Pin `=0.6.x` and wrap the client behind the same `rb_jobs::Enqueue` trait, so a later switch in
  either direction stays local.

Other fallbacks:

- **graphile_worker 0.14** with `driver-tokio-postgres` (264 units, about +21 s CPU, actively
  maintained) is the off-the-shelf option on tokio-postgres. It has no DB-less test path, we must
  supply TLS, and its dedup (`job_key`) is weaker than ByPeriod.
- **apalis + apalis-redis** only if a Valkey job backend becomes a hard requirement.

## Design: our own minimal Postgres queue (`rb-jobs`)

About 1,000 to 1,500 lines with tests. It's driver-agnostic SQL; the examples use tokio-postgres
placeholders.

### Schema

```sql
CREATE TABLE rb_job (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind          text        NOT NULL,
    queue         text        NOT NULL DEFAULT 'default',
    args          jsonb       NOT NULL,
    state         text        NOT NULL DEFAULT 'available'
                  CHECK (state IN ('available', 'running', 'completed', 'discarded', 'cancelled')),
    priority      smallint    NOT NULL DEFAULT 1,          -- lower runs first
    attempt       int         NOT NULL DEFAULT 0,
    max_attempts  int         NOT NULL DEFAULT 25,
    scheduled_at  timestamptz NOT NULL DEFAULT now(),       -- retries/snoozes move this forward
    attempted_at  timestamptz,
    attempted_by  text,                                     -- worker id, for the rescuer and logs
    finalized_at  timestamptz,
    errors        jsonb       NOT NULL DEFAULT '[]',        -- [{attempt, at, error}]
    unique_key    bytea,                                    -- NULL = not unique
    created_at    timestamptz NOT NULL DEFAULT now()
);

-- Fetch path: only rows that can run.
CREATE INDEX rb_job_fetch ON rb_job (queue, priority, scheduled_at, id) WHERE state = 'available';
-- Rescuer path.
CREATE INDEX rb_job_running ON rb_job (attempted_at) WHERE state = 'running';
-- Cleaner path.
CREATE INDEX rb_job_finalized ON rb_job (finalized_at) WHERE finalized_at IS NOT NULL;
-- Uniqueness: NULLed on finalize for "while pending" keys, kept for period keys (see below).
CREATE UNIQUE INDEX rb_job_unique ON rb_job (unique_key) WHERE unique_key IS NOT NULL;

CREATE TABLE rb_periodic (
    name        text PRIMARY KEY,
    next_run_at timestamptz NOT NULL
);
```

"Scheduled" and "retryable" are not separate states. They are `available` rows whose
`scheduled_at` is in the future. That removes River's scheduler service: the fetch query's
`scheduled_at <= now()` does its job.

### Core statements

```sql
-- enqueue (inside the caller's transaction when transactional)
INSERT INTO rb_job (kind, queue, args, priority, max_attempts, scheduled_at, unique_key)
VALUES ($1, $2, $3, $4, $5, coalesce($6, now()), $7)
ON CONFLICT (unique_key) WHERE unique_key IS NOT NULL DO NOTHING
RETURNING id;
SELECT pg_notify('rb_job', $2);   -- delivered only when the transaction commits

-- fetch: claim up to $2 jobs for this worker, never blocking on rows others hold
WITH next AS (
    SELECT id FROM rb_job
    WHERE state = 'available' AND queue = $1 AND scheduled_at <= now()
    ORDER BY priority, scheduled_at, id
    LIMIT $2
    FOR UPDATE SKIP LOCKED
)
UPDATE rb_job j
SET state = 'running', attempt = j.attempt + 1, attempted_at = now(), attempted_by = $3
FROM next WHERE j.id = next.id
RETURNING j.*;

-- complete
UPDATE rb_job SET state = 'completed', finalized_at = now(),
       unique_key = CASE WHEN $2 THEN unique_key END        -- $2 = keep key (period uniqueness)
WHERE id = $1 AND state = 'running';

-- fail: retry or discard, delay computed in Rust from rb_core::retry::Policy
UPDATE rb_job SET
    errors = errors || jsonb_build_object('attempt', attempt, 'at', now(), 'error', $2::text),
    state = CASE WHEN attempt >= max_attempts THEN 'discarded' ELSE 'available' END,
    scheduled_at = CASE WHEN attempt >= max_attempts THEN scheduled_at ELSE now() + $3::interval END,
    finalized_at = CASE WHEN attempt >= max_attempts THEN now() END,
    unique_key = CASE WHEN attempt >= max_attempts AND NOT $4 THEN NULL ELSE unique_key END
WHERE id = $1 AND state = 'running';

-- rescue (any node, idempotent): jobs whose worker died mid-run
UPDATE rb_job SET state = 'available', scheduled_at = now()
WHERE state = 'running' AND attempted_at < now() - $1::interval;

-- periodic tick (any node, exactly once per tick): claim the tick and enqueue in one transaction
UPDATE rb_periodic SET next_run_at = $2 WHERE name = $1 AND next_run_at <= now() RETURNING name;
-- …if a row came back, INSERT the job in the same transaction.

-- cleaner
DELETE FROM rb_job WHERE finalized_at < now() - $1::interval;
```

Uniqueness follows River's `ByArgs` + `ByPeriod`. The key is
`sha256(kind ‖ canonical_json(args) ‖ queue ‖ floor(scheduled_at / period))`, computed with
`ring::digest`, which is already in the tree, so no `sha2` is needed.

- Keys with a period stay on the row after completion, so a re-enqueue in the same window is a
  no-op.
- Keys without a period are cleared on finalize, so they only dedup pending and running jobs.

Rescue and periodic need no leader election: both are single atomic `UPDATE`s that every node may
run. If running them on every node gets noisy, gate them with `pg_try_advisory_lock`.

### Rust API sketch

```rust
pub trait JobArgs: Serialize + DeserializeOwned + Send + 'static {
    const KIND: &'static str;
    fn opts() -> InsertOpts { InsertOpts::default() }    // queue, priority, max_attempts, unique
}

pub trait Worker<A: JobArgs>: Send + Sync + 'static {
    fn work(&self, ctx: JobCtx, job: Job<A>) -> impl Future<Output = Result<Outcome, BoxError>> + Send;
    fn timeout(&self, _job: &Job<A>) -> Duration { Duration::from_secs(120) }
}
pub enum Outcome { Complete, Snooze(Duration), Discard, Cancel }

/// Storage seam: `PgStore` (driver-specific) and `MemStore` (tests).
pub trait Store: Send + Sync + 'static {
    type Tx<'a>;
    fn insert(&self, job: NewJob) -> impl Future<Output = Result<Option<JobId>>> + Send;
    fn insert_tx(&self, tx: &mut Self::Tx<'_>, job: NewJob) -> impl Future<Output = Result<Option<JobId>>> + Send;
    fn fetch(&self, queue: &str, limit: u32, worker: &str) -> impl Future<Output = Result<Vec<RawJob>>> + Send;
    fn complete(&self, id: JobId, keep_unique: bool) -> impl Future<Output = Result<()>> + Send;
    fn fail(&self, id: JobId, err: &str, retry_in: Duration, keep_unique: bool) -> impl Future<Output = Result<()>> + Send;
    fn rescue(&self, older_than: Duration) -> impl Future<Output = Result<u64>> + Send;
    fn tick_periodic(&self, name: &str, next: SystemTime) -> impl Future<Output = Result<bool>> + Send;
    fn wakeups(&self) -> impl Stream<Item = String> + Send;   // LISTEN rb_job; MemStore: a tokio watch
}

let jobs = Jobs::builder(store)
    .queue("default", 10)
    .queue("interactive", 2)
    .worker::<PingArgs>(PingWorker)
    .periodic("baseline", Every(Duration::from_secs(60)).run_on_start(), || BaselineArgs {})
    .rescue_after(Duration::from_secs(300))
    .retry(rb_core::retry::Policy { max_attempts: 25, initial: 1s, max_delay: 1h, multiplier: 4.0, jitter: 0.1 })
    .clock(clock.clone())             // SharedClock: MemStore time, backoff jitter seed, periodic
    .build();
let run = jobs.start(shutdown.clone());   // stop fetching on signal; wait grace; then cancel
```

Runtime:

- **Fetch loop.** One task per queue, gated by a `tokio::sync::Semaphore(max_workers)`. It wakes on
  three triggers: a `LISTEN rb_job` notification (a dedicated connection, `AsyncMessage::Notification`
  in tokio-postgres or `PgListener` in sqlx), a fallback poll interval (1 s), or a completed job
  freeing a permit.
- **Job execution.** Each job runs in its own task under `tokio::time::timeout`, inside a
  `tracing::info_span!("job", kind, queue, id, attempt)`.
- **Observer hooks.** An `Observer` trait (`on_start`, `on_finish(outcome, latency)`) feeds the
  existing `rb_core::stats` latency recorder and the admin traffic view.
- **Shutdown.** Mirrors `jobs.Stop`: close the semaphore to new fetches, wait up to the grace
  period, then cancel in-flight tasks. Cancelled jobs are reset to
  `available, attempt = attempt - 1` so a deploy doesn't consume an attempt.

Testing without a database:

- `MemStore` keeps a `Mutex<BTreeMap<JobId, Row>>` and evaluates "now" through `SharedClock`.
  Tests drive backoff, scheduled jobs, uniqueness windows, periodic ticks and rescue with
  `clock::Fake::advance`, and never sleep.
- The same conformance suite runs against `PgStore` in the e2e binary when `DATABASE_URL` is set.
- Time-sensitive assertions there use explicit `scheduled_at` values, because Postgres uses
  `now()`.

What we give up compared with riverqueue:

- River UI and the job/queue management API (list, retry, cancel, pause).
- Insert and work middleware.
- Resumable steps.
- Mixed deployment with the Go service.
- A battle-tested rescuer and leader election.
- Batch inserts with `COPY`.

Each is buildable, but each is more code we own.

## Risks

| Risk | Applies to | Mitigation |
| --- | --- | --- |
| Pre-1.0 API churn (9 releases in 6 weeks) | riverqueue | Pin `=0.6.x`; wrap it in `rb-jobs` so churn stays in one crate; add a Dependabot group. |
| MPL-2.0 license | riverqueue | Crate-scoped `[[licenses.exceptions]]` in `deny.toml`. Don't vendor or patch its files (patched MPL files must stay MPL). |
| digest/rand duplicates between axum and the Postgres driver | any Postgres choice | `skip` entries with reasons; remove them when axum/tungstenite move to digest 0.11 / rand 0.10. |
| sqlx's `pool.begin()` isn't cancel-safe (the riverqueue README warns about it) | sqlx | Begin transactions with `riverqueue::database::begin_postgres` in request handlers that can be cancelled (tower timeout layer). |
| Postgres bloat from a hot job table | all Postgres queues | River's cleaner + reindexer handle this. Our own design needs the cleaner and autovacuum tuning on `rb_job`. |
| Mixed Go/Rust on one DB needs River Go ≥ 0.48 (migrations 007–008 and ByPeriod key parity) | riverqueue | Only if we ever share the golang-build database. Upgrade Go first, then migrate. |
| Feature drift if we build our own | own queue | Limit it to the Go features we actually use (the table at the top). Reconsider riverqueue at its 1.0. |
| Valkey isn't a durable job store with transactional enqueue | apalis-redis, rusty-sidekiq | Use Valkey for cache, rate limiting and pub/sub only. Jobs live in Postgres next to the app writes. |
| Compile time | all | Own queue on tokio-postgres (no proc-macros). If we choose riverqueue: sqlx without `macros`/`migrate`, no `sqlite`. Either way `rb-jobs` sits behind a feature so `cargo t -p rb-core` etc. never build it. |
| Owning queue correctness (lost or double-run jobs) | own queue | Same SQL as River/graphile (`SKIP LOCKED` claim, rescuer, partial unique index). A conformance suite runs against both `MemStore` and real Postgres. Jobs must be idempotent: delivery is at-least-once, as in River. |

## Sources

Crates (versions and dates from the crates.io API and `cargo info`, 2026-11-21):

- riverqueue: <https://crates.io/crates/riverqueue>, <https://docs.rs/riverqueue/0.6.0>,
  source <https://github.com/riverqueue/river/tree/master/rust>, mixed-deployment guide
  <https://docs.rs/riverqueue/latest/riverqueue/guide/mixed_deployments/index.html>,
  test helpers <https://crates.io/crates/riverqueue-test>, River UI <https://github.com/riverqueue/riverui>,
  River docs <https://riverqueue.com/docs>
- apalis: <https://crates.io/crates/apalis>, <https://github.com/apalis-dev/apalis>,
  <https://crates.io/crates/apalis-postgres>, <https://github.com/apalis-dev/apalis-postgres>,
  <https://crates.io/crates/apalis-redis>, <https://crates.io/crates/apalis-cron>,
  <https://crates.io/crates/apalis-board>, <https://crates.io/crates/apalis-sql> (still 1.0.0-rc.9;
  superseded by apalis-postgres)
- underway: <https://crates.io/crates/underway>, <https://github.com/maxcountryman/underway>
- graphile_worker: <https://crates.io/crates/graphile_worker>,
  <https://github.com/leo91000/graphile_worker_rs>, guide <https://leo91000.github.io/graphile_worker_rs/>
- awa: <https://crates.io/crates/awa>, <https://github.com/hardbyte/awa>
- sqlxmq: <https://crates.io/crates/sqlxmq>, <https://github.com/Diggsey/sqlxmq>
- fang: <https://crates.io/crates/fang>, <https://github.com/ayrat555/fang>
- backie: <https://crates.io/crates/backie>, <https://github.com/rafaelcaricio/backie> (archived)
- rusty-sidekiq: <https://crates.io/crates/rusty-sidekiq>, <https://github.com/film42/sidekiq-rs>
- faktory: <https://crates.io/crates/faktory>, <https://github.com/jonhoo/faktory-rs>
- effectum: <https://crates.io/crates/effectum>, <https://github.com/dimfeld/effectum>
- aide-de-camp: <https://crates.io/crates/aide-de-camp> (repository
  <https://github.com/zero-assumptions/aide-de-camp> returns 404)
- pgmq: <https://crates.io/crates/pgmq>, <https://github.com/pgmq/pgmq>
- pgqueue: <https://crates.io/crates/pgqueue>, <https://github.com/joowani/pgqueue>
- Drivers: <https://crates.io/crates/sqlx>, <https://github.com/launchbadge/sqlx>,
  <https://crates.io/crates/tokio-postgres>, <https://github.com/rust-postgres/rust-postgres>,
  <https://crates.io/crates/deadpool-postgres>, <https://crates.io/crates/tokio-postgres-rustls>,
  <https://crates.io/crates/redis>, <https://crates.io/crates/fred>

Postgres:

- `FOR UPDATE SKIP LOCKED`: <https://www.postgresql.org/docs/current/sql-select.html#SQL-FOR-UPDATE-SHARE>
- `NOTIFY` (delivered at commit): <https://www.postgresql.org/docs/current/sql-notify.html>
- `LISTEN`: <https://www.postgresql.org/docs/current/sql-listen.html>
- Advisory locks: <https://www.postgresql.org/docs/current/explicit-locking.html#ADVISORY-LOCKS>
- `INSERT … ON CONFLICT` with partial-index inference: <https://www.postgresql.org/docs/current/sql-insert.html#SQL-ON-CONFLICT>

Repo evidence:

- golang-build `internal/jobs/client.go` and `jobs.go`, `internal/tracing/jobs.go`,
  `internal/investigations/jobs.go`, `internal/profiling/jobs.go`, `cmd/server/main.go`,
  `go.mod` (River v0.35.1).
- rust-build `Cargo.toml`, `deny.toml`, `AGENTS.md`, `crates/rb-core/src/{clock,retry}.rs`.
