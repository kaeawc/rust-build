//! `rb` — one binary for every non-GUI face of rust-build.

mod cmd;
mod tlsargs;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rb", version, about = "rust-build: server, client, load generator, TLS tunnel")]
struct Cli {
    /// Log filter, e.g. `info`, `rb_server=debug` (also RUST_LOG).
    #[arg(long, global = true, env = "RUST_LOG", default_value = "info")]
    log: String,
    /// Log format: `text` (human) or `json` (one object per line).
    #[arg(long, global = true, env = "RB_LOG_FORMAT", default_value = "text")]
    log_format: rb_telemetry::LogFormat,
    /// Export traces over OTLP/HTTP to this collector (e.g. http://localhost:4318). Needs a build
    /// with `--features otel`.
    #[arg(long, global = true, env = "OTEL_EXPORTER_OTLP_ENDPOINT")]
    otlp_endpoint: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the API + WebSocket server (HTTP/1.1 + HTTP/2, optional TLS).
    Serve(cmd::serve::Args),
    /// Make an HTTP request and print the response.
    Get(cmd::get::Args),
    /// Open a WebSocket, send messages (args or stdin lines) and print replies.
    Ws(cmd::ws::Args),
    /// Generate load against a URL and report throughput and latency percentiles.
    Loadgen(cmd::loadgen::Args),
    /// TLS tunnel: `server` wraps a TCP service in TLS, `client` reaches one through TLS.
    #[command(subcommand)]
    Tunnel(cmd::tunnel::Command),
    /// Write a self-signed development certificate and key.
    Cert(cmd::cert::Args),
    /// Issue or verify HS256 tokens.
    #[command(subcommand)]
    Token(cmd::token::Command),
    /// Write a `.env` for the server from a preset (headless onboarding).
    Env(cmd::env::Args),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let telemetry = rb_telemetry::Config {
        filter: cli.log.clone(),
        format: cli.log_format,
        otlp_endpoint: cli.otlp_endpoint.clone(),
        service_name: "rb".into(),
    };

    // Synchronous commands skip building a runtime entirely (and never export traces).
    if matches!(cli.command, Command::Cert(_) | Command::Token(_) | Command::Env(_)) {
        let _guard = rb_telemetry::init(&rb_telemetry::Config { otlp_endpoint: None, ..telemetry })?;
        return match cli.command {
            Command::Cert(a) => cmd::cert::run(a),
            Command::Token(c) => cmd::token::run(c),
            Command::Env(a) => cmd::env::run(a),
            _ => unreachable!("matched above"),
        };
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        // Inside the runtime: the OTLP exporter sends spans on it. The guard flushes on exit.
        let _guard = rb_telemetry::init(&telemetry)?;
        match cli.command {
            Command::Serve(a) => cmd::serve::run(a).await,
            Command::Get(a) => cmd::get::run(a).await,
            Command::Ws(a) => cmd::ws::run(a).await,
            Command::Loadgen(a) => cmd::loadgen::run(a).await,
            Command::Tunnel(c) => cmd::tunnel::run(c).await,
            Command::Cert(_) | Command::Token(_) | Command::Env(_) => unreachable!("handled above"),
        }
    })
}
