# Fast paths for the inner loop. Everything is plain cargo underneath.
.PHONY: check test lint fmt ci watch run serve serve-tls desktop desktop-test desktop-snap dist docker docker-dev tools

check:            ## type-check everything (no codegen, no link): the inner loop
	cargo check --all-targets

test:             ## run tests with nextest (falls back to cargo test)
	@command -v cargo-nextest >/dev/null && cargo nextest run || cargo test

lint:
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt --all

ci: fmt lint test ## what CI runs, locally

watch:            ## re-check on save (bacon); `bacon test`, `bacon clippy` for other jobs
	bacon

serve:
	cargo run -p rb -- serve

serve-tls:
	cargo run -p rb -- serve --addr 127.0.0.1:8443 --self-signed

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
	cargo binstall -y cargo-nextest bacon sccache
