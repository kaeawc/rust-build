# rust-build

A Rust rewrite of [golang-build](https://github.com/kaeawc/golang-build): the same server, client
tooling and building blocks, plus TLS everywhere, HTTP/2, a TLS tunnel, and a
[Rune](https://github.com/anomalyco/rune) desktop app. The whole thing is set up for a very fast
edit → check → test loop, locally and in CI.

## Layout

| Path | What |
| --- | --- |
| `crates/rb-core` | Dependency-light building blocks: `Clock` (+ `Fake`), id generators, PCG random, retry with backoff + jitter, circuit breaker, token-bucket rate limiter, sharded TTL kv store, sync/async event bus, cursor pagination, latency stats. |
| `crates/rb-tokens` | HS256 JWT-compatible tokens on `ring` (alg pinned, exp/nbf with leeway, clock-injected). |
| `crates/rb-tls` | rustls configs on the `ring` provider: server (optional mTLS), client (webpki roots / custom CA / insecure), self-signed dev certs. |
| `crates/rb-server` | axum app + hyper-util serve loop: HTTP/1.1 and HTTP/2 (ALPN under TLS, h2c in plaintext), WebSockets (incl. RFC 8441 over h2), request ids, traffic stats, health/readiness probes, graceful drain. |
| `crates/rb-client` | Pooled HTTP/1.1 + HTTP/2 client whose connector does TCP + rustls itself, WebSocket client (`ws://`/`wss://`), load-generation engine with live snapshots. |
| `crates/rb-tunnel` | stunnel-style TLS tunnel: server terminates TLS → TCP upstream; client accepts TCP → TLS to the server. HTTP/2 multiplexing (one TLS connection, an extended-CONNECT stream per TCP connection) with fallback to 1:1, per-stream targets checked against an allowlist, SNI routing, idle timeouts/keepalives, mTLS in every mode, `rb_tunnel_*` metrics. |
| `apps/rb` | The `rb` CLI: `serve`, `get`, `ws`, `loadgen`, `tunnel server|client`, `cert`, `token issue|verify`, `env`. |
| `desktop/` | Rune desktop control panel (its own workspace): health + HTTP version, per-route traffic, 5 s load test, WebSocket ping, embedded server. |

## Quick start

```bash
cargo run -p rb -- serve                                  # http://127.0.0.1:8080 (h1 + h2c)
cargo run -p rb -- serve --addr 127.0.0.1:8443 --self-signed   # https, h2 via ALPN
cargo run -p rb -- get -k -i https://127.0.0.1:8443/api/info    # → HTTP/2.0
cargo run -p rb -- ws -k wss://127.0.0.1:8443/ws hello
cargo run -p rb -- loadgen -c 32 -d 10s http://127.0.0.1:8080/healthz
cargo run -p rb -- cert --out certs                       # cert.pem + key.pem for localhost
cargo run -p rb -- tunnel server --listen :9443 --upstream 127.0.0.1:8080 --cert certs/cert.pem --key certs/key.pem
cargo run -p rb -- tunnel client --listen 127.0.0.1:9000 --remote 127.0.0.1:9443 --ca certs/cert.pem
cd desktop && cargo run                                   # desktop app (starts an embedded server)
cd desktop && cargo run -- --snap panel.png --scene demo  # render it headless
```

### Tunnel

Each client connection is one TLS connection (raw mode) unless the client names a `--target`.
Then one TLS + HTTP/2 connection carries every forwarded connection as its own stream (RFC 8441
extended CONNECT, `:protocol rb-tunnel`, path `/tcp/{host}/{port}`). The server dials a target only
if `--upstream`, the SNI route's upstream or an `--allow` rule permits it. CIDR rules are checked
against the resolved addresses. A client whose server can't multiplex falls back to raw mode, and
raw clients work against multiplexing servers.

```bash
# Server: default upstream, extra allowed targets, a second service by SNI with its own cert.
rb tunnel server --listen :9443 --cert certs/cert.pem --key certs/key.pem \
  --upstream 127.0.0.1:8080 --allow 10.0.0.0/8:5432 --allow cache.internal:6379-6380 \
  --route admin.example=127.0.0.1:9090,cert=admin.pem,key=admin-key.pem \
  --idle-timeout 10m --keepalive 30s --metrics-addr 127.0.0.1:9100
# Client: several local ports over one multiplexed connection.
rb tunnel client --remote tunnel.example:9443 --ca certs/cert.pem \
  --listen 127.0.0.1:5432 --target 10.0.0.5:5432 --forward 127.0.0.1:6379=cache.internal:6379
```

Add `--client-ca ca.pem` on the server and `--client-cert/--client-key` on the client for mTLS.
It applies to every mode and route. `--metrics-addr` serves `rb_tunnel_connections`,
`_connections_active`, `_streams`, `_denied`, `_failures`, `_idle_closed` and `_bytes`.
The server also accepts a classic HTTP/2 CONNECT whose authority is the target.

### API

| Route | |
| --- | --- |
| `GET /healthz` | liveness (store ping cached 5 s), `cache-control: no-store` |
| `GET /readyz` | readiness: all probes; 503 once shutdown starts so load balancers drain |
| `GET /api/info` | negotiated HTTP version, peer address, uptime |
| `GET /api/users?pageSize=&cursor=` | cursor-paginated, cached 30 s (`x-cache: hit|miss`) |
| `POST /api/users` | `{"name": ...}`; invalidates the cache |
| `POST /api/token`, `GET /api/me` | issue / verify HS256 bearer tokens |
| `GET /api/admin/traffic` | per-route counts and latency, keyed by matched route |
| `GET /api/admin/healthchecks` | probe results with latency |
| `/ws` | WebSocket echo (HTTP/1.1 upgrade or HTTP/2 extended CONNECT) |

Errors use one envelope: `{"error": {"code": "...", "message": "..."}}`.

## The fast loop

```bash
cargo c        # alias: check --all-targets — no codegen, no link
cargo t        # alias: nextest run
cargo lint     # alias: clippy --all-targets -D warnings
bacon          # re-check on save (t: tests, c: clippy, d: desktop)
make ci        # fmt + lint + test, like CI
make tools     # cargo-binstall → nextest, bacon, sccache (prebuilt, seconds)
```

What makes it fast, and why:

- **Desktop is its own workspace.** Rune is a ~170 MB git dependency with a GPU stack. Excluding
  `desktop/` means root `cargo check`/`test` never fetch it, resolve it, or feature-unify the
  shared crates against it.
- **One feature set per dependency.** Every crate inherits deps from `[workspace.dependencies]`,
  so `cargo test -p rb-core` followed by a full build doesn't rebuild shared deps with different
  features.
- **`ring` everywhere, no aws-lc.** rustls, tokio-rustls and rcgen run on `ring` (default
  features off), so there's no `aws-lc-sys` CMake build in the graph. The HTTP client's connector
  does TCP + rustls itself instead of pulling in reqwest + hyper-rustls + a platform verifier.
- **One version per crate.** `deny.toml` bans duplicate crate versions (each duplicate compiles a
  crate and its subtree twice), aws-lc and OpenSSL; the CI `deny` job enforces it. Versions of
  crates a framework also uses track the framework: `tokio-tungstenite` stays on the version axum
  uses and moves when axum does.
- **Minimal features.** axum, hyper, hyper-util, tokio, tower-http, clap: default features off,
  only what's used switched on.
- **Profiles.** `dev`: `line-tables-only` debuginfo for our crates, none for deps (smaller
  objects, faster links). `ring`/`rustls` are optimized even in dev so TLS tests stay fast.
  `release` keeps incremental + 16 codegen units for quick optimized builds; `dist` (thin LTO,
  1 CGU) is only for shipping. `ci` drops debuginfo and incremental entirely.
- **Linkers.** macOS's ld-prime and Linux x86_64's default rust-lld are already fast; Windows
  MSVC is pointed at rust-lld in `.cargo/config.toml`.
- **Tests.** Doctests are off (each compiles and links its own binary); integration tests live in
  a single `tests/e2e.rs` binary; nextest runs test binaries in parallel. Time is injected via
  `Clock`, retries via `Sleeper`, so unit tests never sleep.
- **sccache (optional).** With `make tools`, `export RUSTC_WRAPPER=sccache` shares compiled deps
  across git worktrees and clones.

CI (`.github/workflows/ci.yml`): `fmt` (no compile, no cache) · `lint` (clippy, check-mode cache)
· `test` (nextest, JUnit) · `build` (release `rb` artifact), each with its own
[rust-cache](https://github.com/Swatinem/rust-cache) key saved only from `main`, `--locked`,
`CARGO_INCREMENTAL=0`, debuginfo-free `ci` profile, prebuilt tools via `taiki-e/install-action`,
and superseded runs cancelled. The desktop workflow builds nightly on macOS, Linux and Windows, and only
when something it depends on changed since the last successful nightly (or on demand).

## Mapping from golang-build

| golang-build | rust-build |
| --- | --- |
| `cmd/server` (gorilla/mux) | `rb serve` / `rb-server` (axum + hyper-util) |
| `internal/handlers`, `internal/middleware`, `internal/httpmw` | `rb-server` routes + tower layers (request id, trace, catch-panic, timeout, gzip, secure headers, traffic) |
| `internal/httpserver`, `internal/shutdown` | `rb_server::serve` + `shutdown_signal` (graceful drain, readiness flip) |
| `internal/healthcheck` | `rb_server::Probe`, parallel with per-probe timeout |
| `cmd/loadgen` | `rb loadgen` / `rb_client::loadgen`, and the desktop app's load card |
| `cmd/onboard --preset --yes` | `rb env --preset` |
| `cmd/admin` TUI | desktop app (traffic, health, embedded server) |
| `internal/clock`, `idgen`, `random`, `retry`, `circuitbreaker`, `ratelimit`, `kv`, `eventbus`, `paginator`, `tokens` | `rb-core` modules, `rb-tokens` |
| — | `rb-tls`, `rb-tunnel`, HTTP/2, `wss://`, mTLS |

Not ported (yet): Postgres/Valkey backends, migrations, River jobs, the web SPA. The server's
`UserStore` trait is the seam for a real database.

## Repository security

- Only collaborators can open issues and pull requests (interaction limit; GitHub caps it at six
  months, so it needs renewing — currently until 2027-05-21).
- `main` can't be deleted, force-pushed, or given merge commits. Pull requests need CI (`fmt`,
  `lint`, `deny`, `test`, `build`) green and squash-merge; repository admins may push directly. Tags are
  immutable.
- Actions: only GitHub-owned actions plus `Swatinem/rust-cache` and `taiki-e/install-action`,
  all pinned to commit SHAs (Dependabot updates the pins); the workflow token is read-only;
  checkout doesn't persist credentials; outside contributors' workflow runs need approval.
- Secret scanning with push protection, Dependabot alerts and security updates, private
  vulnerability reporting ([SECURITY.md](SECURITY.md)).
