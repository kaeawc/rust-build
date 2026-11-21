# syntax=docker/dockerfile:1.7
# Multi-stage build with BuildKit cache mounts: the cargo registry, git checkouts and target dir
# persist between builds, so a source-only change recompiles just the workspace crates.
FROM rust:1.99.1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --locked --profile dist -p rb && cp target/dist/rb /rb

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /rb /rb
EXPOSE 8080
ENV RB_ADDR=0.0.0.0:8080
ENTRYPOINT ["/rb", "serve"]
