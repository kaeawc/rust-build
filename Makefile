# Fast paths for the inner loop. Everything is plain cargo underneath.
.PHONY: check test lint fmt deny ci watch run serve serve-tls desktop desktop-test desktop-snap dist docker docker-dev tools web web-dev web-test web-e2e serve-web types release

check:            ## type-check everything (no codegen, no link): the inner loop
	cargo check --all-targets

test:             ## run tests with nextest (falls back to cargo test)
	@command -v cargo-nextest >/dev/null && cargo nextest run || cargo test

lint:
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt --all

deny:             ## dependency policy: duplicates, banned crates, advisories, licenses
	cargo deny check

ci: fmt lint deny test ## what CI runs, locally

watch:            ## re-check on save (bacon); `bacon test`, `bacon clippy` for other jobs
	bacon

serve:
	cargo run -p rb -- serve

serve-tls:
	cargo run -p rb -- serve --addr 127.0.0.1:8443 --self-signed

web:              ## build the web UI into web/dist (embedded by `--features web`)
	cd web && bun install --frozen-lockfile && bun run build

web-dev:          ## Vite dev server on :5173, proxying /api, /healthz, /readyz, /ws to `make serve`
	cd web && bun run dev

web-test:         ## web lint, types, unit tests with coverage gate, dead code
	cd web && bun run validate

serve-web: web    ## rb serve with the embedded UI on http://127.0.0.1:8080
	cargo run -p rb --features web -- serve

web-e2e: web      ## Playwright + axe against `rb serve --features web`
	cargo build --profile ci -p rb --features web
	cd web && RB_BIN=../target/ci/rb bun run e2e

types:            ## regenerate web/src/api/gen from the Rust DTOs (ts-rs); CI fails on drift
	rm -rf web/src/api/gen
	TS_RS_EXPORT_DIR=$(CURDIR)/web/src/api/gen TS_RS_LARGE_INT=number cargo test -q -p rb-server -p rb-core -p rb-tokens --features ts export_bindings

release:          ## bump to VERSION, run the checks, commit and tag (push: git push --atomic origin main vVERSION)
	@test -n "$(VERSION)" || { echo "usage: make release VERSION=x.y.z" >&2; exit 2; }
	scripts/release/bump.sh $(VERSION)

desktop:
	cd desktop && cargo run

desktop-test:
	cd desktop && cargo test

desktop-snap:     ## render the desktop app headless to panel.png
	cd desktop && cargo run -- --snap ../panel.png --scene demo

dist:             ## smallest, fastest binary (thin LTO, 1 CGU)
	cargo build --profile dist -p rb

docker:           ## shipping image (thin LTO)
	docker build -f ci/Dockerfile -t rust-build:latest .

docker-dev:       ## quick local image (release profile, incremental)
	docker build -f ci/Dockerfile --build-arg PROFILE=release -t rust-build:dev .

tools:            ## optional dev tools, as prebuilt binaries
	@command -v cargo-binstall >/dev/null || curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
	cargo binstall -y cargo-nextest bacon sccache cargo-deny
