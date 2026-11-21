use crate::tlsargs::{ClientTls, ServerTls};
use std::sync::Arc;
use std::time::Duration;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Terminate TLS on `--listen` and forward plaintext to `--upstream`.
    Server {
        #[arg(long, default_value = "0.0.0.0:8443")]
        listen: String,
        /// Plaintext TCP service to forward to, `host:port`.
        #[arg(long)]
        upstream: String,
        #[command(flatten)]
        tls: ServerTls,
    },
    /// Accept plaintext on `--listen` and forward it over TLS to `--remote`.
    Client {
        #[arg(long, default_value = "127.0.0.1:9000")]
        listen: String,
        /// Tunnel server, `host:port`.
        #[arg(long)]
        remote: String,
        /// Certificate name to verify (defaults to the remote host).
        #[arg(long)]
        server_name: Option<String>,
        #[command(flatten)]
        tls: ClientTls,
    },
}

async fn report(stats: Arc<rb_tunnel::Stats>) {
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    let mut last = stats.snapshot();
    loop {
        tick.tick().await;
        let now = stats.snapshot();
        if now != last {
            tracing::info!(
                accepted = now.accepted,
                active = now.active,
                failed = now.failed,
                up = now.bytes_up,
                down = now.bytes_down,
                "tunnel stats"
            );
            last = now;
        }
    }
}

pub async fn run(c: Command) -> anyhow::Result<()> {
    let stats = Arc::new(rb_tunnel::Stats::default());
    let reporter = tokio::spawn(report(stats.clone()));
    let result = match c {
        Command::Server { listen, upstream, tls } => {
            anyhow::ensure!(tls.enabled(), "tunnel server needs --cert/--key or --self-signed");
            let cfg = tls.config(&[])?.ok_or_else(|| anyhow::anyhow!("no tls config"))?;
            let listener = tokio::net::TcpListener::bind(&listen).await?;
            tracing::info!(listen = %listener.local_addr()?, %upstream, mtls = tls.client_ca.is_some(), "tunnel server up");
            let opts = rb_tunnel::ServerOptions {
                tls: cfg,
                upstream,
                handshake_timeout: Duration::from_secs(10),
                drain_timeout: Duration::from_secs(10),
            };
            rb_tunnel::run_server(listener, opts, stats, rb_server::shutdown_signal()).await
        }
        Command::Client { listen, remote, server_name, tls } => {
            let cfg = rb_tls::client_config(tls.options()?)?;
            let listener = tokio::net::TcpListener::bind(&listen).await?;
            tracing::info!(listen = %listener.local_addr()?, %remote, "tunnel client up");
            let opts = rb_tunnel::ClientOptions { tls: cfg, remote, server_name, drain_timeout: Duration::from_secs(10) };
            rb_tunnel::run_client(listener, opts, stats, rb_server::shutdown_signal()).await
        }
    };
    reporter.abort();
    Ok(result?)
}
