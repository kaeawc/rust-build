# rust-build

Rust port of golang-build: API server, CLI clients, TLS tunnel, and a Rune desktop app.

## Working rules

- Push directly to `main`.
- New dependencies go in root `[workspace.dependencies]` with `default-features = false` and only
  the features used; crates reference them with `{ workspace = true }`. Never add a crate that
  pulls `aws-lc-rs`/`aws-lc-sys` — TLS stays on `ring` (`cargo tree -i aws-lc-rs` must fail).
- Time goes through `rb_core::clock::Clock`, sleeps in retry through `Sleeper`: tests use fakes,
  never real waits.
- Keep integration tests in `apps/rb/tests/e2e.rs` (one binary). Prefer unit tests in-module.
- Doctests are disabled; don't rely on them for coverage.
- `desktop/` is a separate workspace. Bump the Rune `rev` and every `[patch.crates-io]` entry
  together. After changing root dependencies, run `cd desktop && cargo check` and commit
  `desktop/Cargo.lock`.

## Build & validate

```bash
cargo c                      # check --all-targets
cargo t                      # nextest run (or: cargo test)
cargo lint                   # clippy --all-targets -D warnings
cargo fmt --all
cd desktop && cargo test     # Rune headless view tests
```

After any change: `cargo c && cargo lint && cargo t`.

## Map

- `crates/rb-core` — clock, idgen, random, retry, circuitbreaker, ratelimit, kv, eventbus, paginator, stats.
- `crates/rb-tokens` — HS256 tokens.
- `crates/rb-tls` — rustls (ring) server/client configs, self-signed certs.
- `crates/rb-server` — axum router (`api.rs`), serve loop (`serve.rs`), state/probes, traffic recorder, `UserStore`.
- `crates/rb-client` — `HttpClient` (custom TCP+TLS connector, ALPN h2), `ws`, `loadgen`.
- `crates/rb-tunnel` — TLS tunnel server/client.
- `apps/rb` — CLI; one module per subcommand under `src/cmd/`.
- `desktop/` — Rune app: `backend.rs` (network on a shared tokio runtime), `view.rs`, `main.rs` (state, actions, poll, tests).
