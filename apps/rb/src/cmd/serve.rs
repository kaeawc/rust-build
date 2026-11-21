use crate::tlsargs::ServerTls;
use std::time::Duration;

#[derive(clap::Args)]
pub struct Args {
    /// Address to listen on.
    #[arg(long, env = "RB_ADDR", default_value = "127.0.0.1:8080")]
    addr: String,
    /// HMAC secret for `/api/token` and `/api/me`.
    #[arg(long, env = "RB_TOKEN_SECRET", default_value = "dev-secret-change-me", hide_env_values = true)]
    token_secret: String,
    /// Grace period for in-flight requests on shutdown.
    #[arg(long, value_parser = super::duration, default_value = "10s")]
    shutdown_timeout: Duration,
    #[command(flatten)]
    tls: ServerTls,
}

pub async fn run(a: Args) -> anyhow::Result<()> {
    let tls = a.tls.config(&[rb_tls::ALPN_H2, rb_tls::ALPN_HTTP11])?;
    let state = rb_server::AppState::new(rb_server::Config { token_secret: a.token_secret.into_bytes(), ..Default::default() });
    let app = rb_server::router(state.clone());
    let listener = tokio::net::TcpListener::bind(&a.addr).await?;
    let scheme = if tls.is_some() { "https" } else { "http" };
    tracing::info!(addr = %listener.local_addr()?, "listening on {scheme}://{} (h1 + h2{})", listener.local_addr()?, if tls.is_some() { "" } else { "c" });

    let shutdown = async move {
        rb_server::shutdown_signal().await;
        tracing::info!("shutdown requested; failing readiness and draining");
        state.set_shutting_down();
    };
    rb_server::serve(listener, app, rb_server::ServeOptions { tls, shutdown_timeout: a.shutdown_timeout, ..Default::default() }, shutdown)
        .await?;
    Ok(())
}
