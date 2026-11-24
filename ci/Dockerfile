# syntax=docker/dockerfile:1.7
# Multi-stage build with BuildKit cache mounts: the cargo registry, git checkouts and target dir
# persist between builds, so a source-only change recompiles just the workspace crates.

# The web UI (web/dist), embedded into rb by `--features web` (part of `full`).
FROM oven/bun:1.4.5-slim@sha256:b780593f3e10918cbdaec3d5a08d52224b9198069a65eb47812085c2ec007643 AS web
WORKDIR /web
COPY web/package.json web/bun.lock ./
RUN --mount=type=cache,target=/root/.bun/install/cache bun install --frozen-lockfile
COPY web/ ./
RUN bun run build

FROM rust:1.99.1-slim-bookworm AS build
# `dist` (thin LTO, 1 CGU) for shipping; `--build-arg PROFILE=release` for quick local images.
ARG PROFILE=dist
# What releases ship: OpenTelemetry, Postgres, Valkey and the web UI. `--build-arg FEATURES=` for none.
ARG FEATURES=full
WORKDIR /src
COPY . .
COPY --from=web /web/dist web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --locked --profile "$PROFILE" -p rb --features "$FEATURES" && cp "target/$PROFILE/rb" /rb

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /rb /rb
EXPOSE 8080
ENV RB_ADDR=0.0.0.0:8080
ENTRYPOINT ["/rb", "serve"]
