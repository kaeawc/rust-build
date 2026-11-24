use crate::tlsargs::{ClientTls, ServerTls};
use anyhow::Context as _;
use rb_telemetry::metrics::Metrics;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Terminate TLS on `--listen`: raw connections go to `--upstream` (or the SNI route's
    /// upstream); multiplexed (HTTP/2) streams go to the targets they ask for, if `--allow`ed.
    Server {
        #[arg(long, default_value = "0.0.0.0:8443")]
        listen: String,
        /// Plaintext TCP service to forward raw connections to, `host:port`. Always allowed as a
        /// multiplexed target too.
        #[arg(long)]
        upstream: Option<String>,
        /// Allow multiplexed streams to dial these targets: `host:port`, `host:lo-hi`,
        /// `10.0.0.0/8:5432`, `[fd00::/8]:*`. Repeatable. CIDRs are checked after DNS.
        #[arg(long = "allow", value_name = "RULE")]
        allow: Vec<String>,
        /// Route by SNI: `NAME=UPSTREAM[,cert=PEM,key=PEM]` (UPSTREAM may be empty to use only
        /// the certificate). Repeatable. `--client-ca` applies to every route.
        #[arg(long = "route", value_name = "SNI=UPSTREAM")]
        routes: Vec<String>,
        /// Don't advertise HTTP/2: every client uses one TLS connection per forwarded connection.
        #[arg(long)]
        no_multiplex: bool,
        #[command(flatten)]
        common: Common,
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
        /// Certificate name to verify, and the SNI sent (defaults to the remote host).
        #[arg(long)]
        server_name: Option<String>,
        /// Target the server should dial for connections on `--listen` (multiplexed mode).
        #[arg(long)]
        target: Option<String>,
        /// More listeners on the same tunnel connection: `LISTEN=HOST:PORT`. Repeatable.
        #[arg(long = "forward", value_name = "LISTEN=TARGET")]
        forwards: Vec<String>,
        /// `auto` multiplexes when a target is set; `h2` requires it; `raw` never multiplexes.
        /// Multiplexing falls back to raw when the server doesn't support it.
        #[arg(long, default_value = "auto")]
        mode: rb_tunnel::Mode,
        #[command(flatten)]
        common: Common,
        #[command(flatten)]
        tls: ClientTls,
    },
}

#[derive(clap::Args)]
pub struct Common {
    /// Close a connection or stream after this long with no bytes in either direction.
    #[arg(long, value_parser = super::duration)]
    idle_timeout: Option<Duration>,
    /// Send HTTP/2 PINGs this often on multiplexed connections; a peer that misses one is dropped.
    #[arg(long, value_parser = super::duration)]
    keepalive: Option<Duration>,
    /// Serve Prometheus metrics (`rb_tunnel_*`) at `http://ADDR/metrics`.
    #[arg(long)]
    metrics_addr: Option<String>,
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
                streams = now.streams,
                denied = now.denied,
                up = now.bytes_up,
                down = now.bytes_down,
                "tunnel stats"
            );
            last = now;
        }
    }
}

/// `NAME=UPSTREAM[,cert=PEM,key=PEM]`.
fn parse_route(s: &str, tls: &ServerTls) -> anyhow::Result<rb_tunnel::Route> {
    let (sni, rest) = s.split_once('=').with_context(|| format!("--route {s:?}: want NAME=UPSTREAM[,cert=PEM,key=PEM]"))?;
    anyhow::ensure!(!sni.is_empty(), "--route {s:?}: empty server name");
    let mut parts = rest.split(',');
    let upstream = parts.next().filter(|u| !u.is_empty()).map(str::to_string);
    let (mut cert, mut key) = (None, None);
    for p in parts {
        match p.split_once('=') {
            Some(("cert", v)) => cert = Some(PathBuf::from(v)),
            Some(("key", v)) => key = Some(PathBuf::from(v)),
            _ => anyhow::bail!("--route {s:?}: unknown option {p:?} (cert=, key=)"),
        }
    }
    let route_tls = match (cert, key) {
        (Some(c), Some(k)) => Some(tls.config_for(rb_tls::Identity::from_pem_files(&c, &k)?, &[])?),
        (None, None) => None,
        _ => anyhow::bail!("--route {s:?}: cert= and key= go together"),
    };
    if let Some(u) = &upstream {
        rb_tunnel::target::split_target(u).with_context(|| format!("--route {s:?}: upstream must be host:port"))?;
    }
    Ok(rb_tunnel::Route { sni: sni.to_string(), upstream, tls: route_tls })
}

async fn serve_metrics(addr: &str, metrics: Arc<Metrics>) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("--metrics-addr {addr}"))?;
    tracing::info!(addr = %listener.local_addr()?, "metrics at /metrics");
    let app = rb_server::metrics_router(metrics);
    Ok(tokio::spawn(async move {
        if let Err(e) = rb_server::serve(listener, app, rb_server::ServeOptions::default(), std::future::pending()).await {
            tracing::warn!(error = %e, "metrics server stopped");
        }
    }))
}

pub async fn run(c: Command) -> anyhow::Result<()> {
    let (side, metrics_addr) = match &c {
        Command::Server { common, .. } => ("server", common.metrics_addr.clone()),
        Command::Client { common, .. } => ("client", common.metrics_addr.clone()),
    };
    let metrics = Arc::new(Metrics::new("rb", env!("CARGO_PKG_VERSION")));
    let stats = Arc::new(rb_tunnel::Stats::with_metrics(metrics.clone(), side));
    let metrics_task = match &metrics_addr {
        Some(addr) => Some(serve_metrics(addr, metrics).await?),
        None => None,
    };
    let reporter = tokio::spawn(report(stats.clone()));
    let result = match c {
        Command::Server { listen, upstream, allow, routes, no_multiplex, common, tls } => {
            anyhow::ensure!(tls.enabled(), "tunnel server needs --cert/--key or --self-signed");
            let cfg = tls.config(&[])?.ok_or_else(|| anyhow::anyhow!("no tls config"))?;
            let routes = routes.iter().map(|r| parse_route(r, &tls)).collect::<anyhow::Result<Vec<_>>>()?;
            let allow = rb_tunnel::Allowlist::parse(&allow)?;
            if let Some(u) = &upstream {
                rb_tunnel::target::split_target(u).with_context(|| format!("--upstream {u:?} must be host:port"))?;
            }
            anyhow::ensure!(
                upstream.is_some() || !routes.is_empty() || !allow.is_empty(),
                "tunnel server needs --upstream, a --route or an --allow rule"
            );
            let listener = tokio::net::TcpListener::bind(&listen).await?;
            tracing::info!(
                listen = %listener.local_addr()?,
                upstream = upstream.as_deref().unwrap_or("-"),
                routes = routes.len(),
                multiplex = !no_multiplex,
                mtls = tls.client_ca.is_some(),
                "tunnel server up"
            );
            let mut opts = rb_tunnel::ServerOptions::forward(cfg, String::new());
            opts.upstream = upstream;
            opts.routes = routes;
            opts.allow = allow;
            opts.multiplex = !no_multiplex;
            opts.idle_timeout = common.idle_timeout;
            opts.keepalive = common.keepalive;
            rb_tunnel::run_server(listener, opts, stats, rb_server::shutdown_signal()).await
        }
        Command::Client { listen, remote, server_name, target, forwards, mode, common, tls } => {
            let cfg = rb_tls::client_config(tls.options()?)?;
            let mut opts = rb_tunnel::ClientOptions::new(cfg, remote.clone());
            opts.server_name = server_name;
            opts.mode = if mode == rb_tunnel::Mode::Auto && !forwards.is_empty() { rb_tunnel::Mode::H2 } else { mode };
            // --forward entries share the main listener's connector, so its target must be valid
            // for the mode too: use the first forward's target for validation if none was given.
            opts.target = target.clone().or_else(|| forwards.first().and_then(|f| f.split_once('=')).map(|(_, t)| t.to_string()));
            opts.keepalive = common.keepalive;
            let connector = opts.connector()?;
            let mut listeners = vec![(listen, target)];
            for f in &forwards {
                let (l, t) = f.split_once('=').with_context(|| format!("--forward {f:?}: want LISTEN=HOST:PORT"))?;
                rb_tunnel::target::split_target(t).with_context(|| format!("--forward {f:?}: target must be host:port"))?;
                listeners.push((l.to_string(), Some(t.to_string())));
            }
            let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
            let mut tasks = tokio::task::JoinSet::new();
            for (listen, target) in listeners {
                let listener = tokio::net::TcpListener::bind(&listen).await.with_context(|| format!("listen {listen}"))?;
                tracing::info!(listen = %listener.local_addr()?, %remote, target = target.as_deref().unwrap_or("(server's upstream)"), "tunnel client up");
                let fwd = rb_tunnel::ForwardOptions { drain_timeout: Duration::from_secs(10), idle_timeout: common.idle_timeout };
                let mut stop = stop_rx.clone();
                tasks.spawn(rb_tunnel::run_forward(listener, connector.clone(), target, fwd, stats.clone(), async move {
                    let _ = stop.wait_for(|s| *s).await;
                }));
            }
            rb_server::shutdown_signal().await;
            let _ = stop_tx.send(true);
            let mut result = Ok(());
            while let Some(r) = tasks.join_next().await {
                if let Ok(Err(e)) = r {
                    result = Err(e);
                }
            }
            result
        }
    };
    reporter.abort();
    if let Some(t) = metrics_task {
        t.abort();
    }
    Ok(result?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_parse_with_and_without_certificates() {
        let tls = ServerTls::default();
        let r = parse_route("api.example=127.0.0.1:8080", &tls).expect("route");
        assert_eq!((r.sni.as_str(), r.upstream.as_deref(), r.tls.is_some()), ("api.example", Some("127.0.0.1:8080"), false));
        let r = parse_route("cert-only.example=", &tls).expect("route");
        assert_eq!(r.upstream, None);
        for bad in ["noequals", "=127.0.0.1:1", "a=b", "a=127.0.0.1:1,cert=x.pem", "a=127.0.0.1:1,bogus=1"] {
            assert!(parse_route(bad, &tls).is_err(), "{bad}");
        }
    }
}
