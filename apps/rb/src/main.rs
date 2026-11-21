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
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_new(&cli.log).unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .compact()
        .init();

    // Synchronous commands skip building a runtime entirely.
    match cli.command {
        Command::Cert(a) => return cmd::cert::run(a),
        Command::Token(c) => return cmd::token::run(c),
        Command::Env(a) => return cmd::env::run(a),
        _ => {}
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
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
