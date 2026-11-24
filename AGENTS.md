# rust-build

Rust port of golang-build: API server, CLI clients, TLS tunnel, and a Rune desktop app.

## Working rules

- Push directly to `main`.
- New dependencies go in root `[workspace.dependencies]` with `default-features = false` and only
  the features used; crates reference them with `{ workspace = true }`. Never add a crate that
  pulls `aws-lc-rs`/`aws-lc-sys` — TLS stays on `ring` (`cargo tree -i aws-lc-rs` must fail).
- One version of every crate (`deny.toml`, enforced by the CI `deny` job). A duplicate compiles
  a crate and its subtree twice. If a bump would duplicate a crate a framework also uses (e.g.
  `tokio-tungstenite`, which axum depends on), keep our version matched to the framework's and
  bump both together when the framework moves; add a Dependabot `ignore` so it stops proposing
  it. Only add a `skip` in `deny.toml` for duplicates we can't control, with a reason.
- Time goes through `rb_core::clock::Clock`, sleeps in retry through `Sleeper`: tests use fakes,
  never real waits.
- Keep integration tests in `apps/rb/tests/e2e.rs` (one binary). Prefer unit tests in-module.
- Doctests are disabled; don't rely on them for coverage.
- Web: schedule through `Timer` (`web/src/lib/timer.ts`), never bare `setTimeout`; pages take the
  `ApiClient` interface, and tests use `test/fakes` instead of mocking libraries. After changing a
  serialized DTO, run `make types` and commit `web/src/api/gen`. Add shadcn components with
  `bunx shadcn add <name>` (the project is initialised with `-b radix`).
- `desktop/` is a separate workspace. Bump the Rune `rev` and every `[patch.crates-io]` entry
  together. After changing root dependencies, run `cd desktop && cargo check` and commit
  `desktop/Cargo.lock`.

## Build & validate

```bash
cargo c                      # check --all-targets
cargo t                      # nextest run (or: cargo test)
cargo lint                   # clippy --all-targets -D warnings
cargo fmt --all
cargo deny check             # dependency policy (cargo binstall cargo-deny)
cd desktop && cargo test     # Rune headless view tests
make web-test                # web: lint, types, unit tests + coverage gate, knip, build (bun)
make web-e2e                 # web: Playwright + axe against rb serve --features web
```

After any change: `cargo c && cargo lint && cargo t`.

## Map

- `crates/rb-core` — clock, idgen, random, retry, circuitbreaker, ratelimit, kv, eventbus, paginator, stats.
- `crates/rb-tokens` — HS256 tokens.
- `crates/rb-tls` — rustls (ring) server/client configs, self-signed certs.
- `crates/rb-server` — axum router (`api.rs`), serve loop (`serve.rs`), state/probes, traffic recorder, `UserStore`.
- `crates/rb-client` — `HttpClient` (custom TCP+TLS connector, ALPN h2), `ws`, `loadgen`.
- `crates/rb-tunnel` — TLS tunnel server/client: raw 1:1 and HTTP/2-multiplexed modes (`server.rs`, `client.rs`), target allowlist (`allow.rs`), stream path codec (`target.rs`), relay + idle timeout (`io.rs`).
- `apps/rb` — CLI; one module per subcommand under `src/cmd/`.
- `crates/rb-web` — embeds `web/dist` (feature `web` on `apps/rb`); `rb_server::router_with_fallback` mounts it.
- `web/` — TypeScript SPA (bun 1.4.5): `src/routes/` (TanStack file routes), `src/api/` (client behind
  an `ApiClient` interface; `gen/` is ts-rs output, never hand-edited), `src/components/ui/` (vendored
  shadcn, `-b radix`), `test/` (bun:test + fakes), `e2e/` (Playwright + axe against `rb serve`).
- `desktop/` — Rune app: `backend.rs` (network on a shared tokio runtime), `view.rs`, `main.rs` (state, actions, poll, tests).
