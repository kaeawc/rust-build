//! TLS tunnelling, stunnel/ghostunnel style, with optional HTTP/2 multiplexing.
//!
//! ```text
//!  app ──tcp──▶ [client: listen 127.0.0.1:5432] ══tls══▶ [server: listen :8443] ──tcp──▶ upstream
//! ```
//!
//! Two wire modes share one server listener, picked per connection by ALPN:
//!
//! - **raw** (no ALPN): one TLS connection per forwarded TCP connection, relayed to the server's
//!   fixed upstream (or the upstream of the SNI route the client named).
//! - **h2** (ALPN `h2`): one TLS connection carries many streams. Each stream is an HTTP/2
//!   extended CONNECT (RFC 8441) naming its own `host:port` target (see [`target`]); the server
//!   dials it only if its [`Allowlist`] allows it.
//!
//! [`run_server`] terminates TLS (optionally requiring client certificates in every mode) and can
//! pick the certificate and upstream by SNI ([`Route`]). [`run_client`] / [`run_forward`] accept
//! plaintext locally and forward through a [`Connector`], which multiplexes when it can and falls
//! back to raw mode when the server doesn't speak h2 or extended CONNECT. Both stop accepting when
//! `shutdown` resolves and let open connections drain; [`Stats`] counts connections, streams and
//! bytes, and can feed the `rb_tunnel_*` Prometheus families.

pub mod allow;
mod client;
mod io;
mod server;
pub mod target;

pub use allow::{Allowlist, Denied, Rule};
pub use client::{ClientOptions, Connector, ConnectorOptions, ForwardOptions, Mode, run_client, run_forward};
pub use io::{BoxStream, IdleTracker, Stream, pipe};
pub use server::{Route, ServerOptions, run_server};

use rb_telemetry::metrics::{DirectionLabels, Metrics, TunnelLabels};
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;

/// Counters for one side of a tunnel. With [`Stats::with_metrics`], every update also goes to the
/// `rb_tunnel_*` families, labelled with the side.
#[derive(Default)]
pub struct Stats {
    /// Connections accepted: local TCP on the client, TLS on the server.
    pub accepted: AtomicU64,
    pub active: AtomicU64,
    pub failed: AtomicU64,
    /// Bytes from the accepting side towards the far side.
    pub bytes_up: AtomicU64,
    /// Bytes from the far side back to the accepting side.
    pub bytes_down: AtomicU64,
    /// Multiplexed streams opened (h2 mode).
    pub streams: AtomicU64,
    /// Streams refused by the allowlist.
    pub denied: AtomicU64,
    /// Streams closed by the idle timeout.
    pub idle_closed: AtomicU64,
    metrics: Option<(Arc<Metrics>, String)>,
}

impl std::fmt::Debug for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stats").field("snapshot", &self.snapshot()).finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub accepted: u64,
    pub active: u64,
    pub failed: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub streams: u64,
    pub denied: u64,
    pub idle_closed: u64,
}

impl Stats {
    /// Stats that also record into `metrics` with `side` = `server` or `client`.
    pub fn with_metrics(metrics: Arc<Metrics>, side: &str) -> Self {
        Stats { metrics: Some((metrics, side.to_string())), ..Default::default() }
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            accepted: self.accepted.load(Ordering::Relaxed),
            active: self.active.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            bytes_up: self.bytes_up.load(Ordering::Relaxed),
            bytes_down: self.bytes_down.load(Ordering::Relaxed),
            streams: self.streams.load(Ordering::Relaxed),
            denied: self.denied.load(Ordering::Relaxed),
            idle_closed: self.idle_closed.load(Ordering::Relaxed),
        }
    }

    fn labels(&self) -> Option<(&Metrics, TunnelLabels)> {
        self.metrics.as_ref().map(|(m, side)| (&**m, TunnelLabels { side: side.clone() }))
    }

    fn record_accept(&self) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
        self.active.fetch_add(1, Ordering::Relaxed);
        if let Some((m, l)) = self.labels() {
            m.tunnel_connections.get_or_create(&l).inc();
            m.tunnel_active.get_or_create(&l).inc();
        }
    }

    fn record_close(&self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
        if let Some((m, l)) = self.labels() {
            m.tunnel_active.get_or_create(&l).dec();
        }
    }

    fn record_failure(&self) {
        self.failed.fetch_add(1, Ordering::Relaxed);
        if let Some((m, l)) = self.labels() {
            m.tunnel_failures.get_or_create(&l).inc();
        }
    }

    fn record_stream(&self) {
        self.streams.fetch_add(1, Ordering::Relaxed);
        if let Some((m, l)) = self.labels() {
            m.tunnel_streams.get_or_create(&l).inc();
        }
    }

    fn record_denied(&self) {
        self.denied.fetch_add(1, Ordering::Relaxed);
        if let Some((m, l)) = self.labels() {
            m.tunnel_denied.get_or_create(&l).inc();
        }
    }

    fn record_idle_close(&self) {
        self.idle_closed.fetch_add(1, Ordering::Relaxed);
        if let Some((m, l)) = self.labels() {
            m.tunnel_idle_closed.get_or_create(&l).inc();
        }
    }

    fn record_bytes(&self, up: u64, down: u64) {
        self.bytes_up.fetch_add(up, Ordering::Relaxed);
        self.bytes_down.fetch_add(down, Ordering::Relaxed);
        if let Some((m, side)) = &self.metrics {
            let dir = |d: &str| DirectionLabels { side: side.clone(), direction: d.into() };
            m.tunnel_bytes.get_or_create(&dir("up")).inc_by(up);
            m.tunnel_bytes.get_or_create(&dir("down")).inc_by(down);
        }
    }
}

fn host_of(addr: &str) -> &str {
    match addr.rsplit_once(':') {
        Some((host, _)) => host.trim_start_matches('[').trim_end_matches(']'),
        None => addr,
    }
}

/// Accepts until `shutdown`, running `handle` per connection. `handle` gets a receiver that
/// flips to `true` when shutdown starts (multiplexed connections use it to send GOAWAY), then
/// connections get `drain` to finish before they're aborted.
async fn accept_loop<F, Fut>(
    listener: TcpListener,
    stats: Arc<Stats>,
    shutdown: impl Future<Output = ()>,
    drain: Duration,
    handle: F,
) -> std::io::Result<()>
where
    F: Fn(TcpStream, SocketAddr, Arc<Stats>, watch::Receiver<bool>) -> Fut,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    let mut shutdown = std::pin::pin!(shutdown);
    let (draining_tx, draining) = watch::channel(false);
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (tcp, peer) = match accepted {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let _ = tcp.set_nodelay(true);
                stats.record_accept();
                let fut = handle(tcp, peer, stats.clone(), draining.clone());
                let stats = stats.clone();
                conns.spawn(async move {
                    if let Err(e) = fut.await {
                        stats.record_failure();
                        tracing::debug!(%peer, error = %e, "tunnel connection failed");
                    }
                    stats.record_close();
                });
            }
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
            () = &mut shutdown => break,
        }
    }
    drop(listener);
    let _ = draining_tx.send(true);
    let drained = tokio::time::timeout(drain, async { while conns.join_next().await.is_some() {} }).await;
    if drained.is_err() {
        tracing::warn!(remaining = conns.len(), "drain timeout; closing tunnel connections");
        conns.abort_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;
    use tokio_rustls::TlsConnector;

    async fn echo_upstream() -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = l.local_addr().expect("addr");
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        addr
    }

    async fn wait_idle(stats: &[&Stats]) {
        for _ in 0..200 {
            if stats.iter().all(|s| s.snapshot().active == 0) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bytes_round_trip_through_client_and_server_with_mtls_in_both_modes() {
        let upstream = echo_upstream().await;
        let ss = rb_tls::self_signed(&["localhost".into()]).expect("cert");
        let server_tls = rb_tls::server_config_mtls(ss.identity.clone(), &ss.identity.certs, &[]).expect("server cfg");
        let client_tls = rb_tls::client_config(rb_tls::ClientOptions {
            trust: rb_tls::Trust::Custom(ss.identity.certs.clone()),
            identity: Some(ss.identity.clone()),
            alpn: vec![],
        })
        .expect("client cfg");

        let server_l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let server_addr = server_l.local_addr().expect("addr");
        let (stop_s, stop_s_rx) = oneshot::channel::<()>();
        let server_stats = Arc::new(Stats::default());
        let s = tokio::spawn(run_server(server_l, ServerOptions::forward(server_tls, upstream.to_string()), server_stats.clone(), async {
            let _ = stop_s_rx.await;
        }));

        for (mode, target) in [(Mode::Raw, None), (Mode::H2, Some(upstream.to_string()))] {
            let client_l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let client_addr = client_l.local_addr().expect("addr");
            let (stop_c, stop_c_rx) = oneshot::channel::<()>();
            let client_stats = Arc::new(Stats::default());
            let mut opts = ClientOptions::new(client_tls.clone(), server_addr.to_string());
            opts.server_name = Some("localhost".into());
            opts.mode = mode;
            opts.target = target;
            let c = tokio::spawn(run_client(client_l, opts, client_stats.clone(), async {
                let _ = stop_c_rx.await;
            }));

            let mut conn = TcpStream::connect(client_addr).await.expect("connect");
            conn.write_all(b"hello through tls").await.expect("write");
            let mut buf = [0u8; 17];
            conn.read_exact(&mut buf).await.expect("read");
            assert_eq!(&buf, b"hello through tls", "{mode:?}");
            drop(conn);

            wait_idle(&[&client_stats]).await;
            let _ = stop_c.send(());
            let _ = c.await;
            let cs = client_stats.snapshot();
            assert_eq!((cs.accepted, cs.failed, cs.bytes_up, cs.bytes_down), (1, 0, 17, 17), "{mode:?}");
            assert_eq!(cs.streams, u64::from(mode == Mode::H2), "{mode:?}");
        }
        let _ = stop_s.send(());
        let _ = s.await;
        let ss = server_stats.snapshot();
        assert_eq!((ss.accepted, ss.streams), (2, 1));
    }

    #[tokio::test]
    async fn server_rejects_clients_without_a_certificate() {
        let ss = rb_tls::self_signed(&["localhost".into()]).expect("cert");
        let server_tls = rb_tls::server_config_mtls(ss.identity.clone(), &ss.identity.certs, &[]).expect("server cfg");
        let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = l.local_addr().expect("addr");
        let stats = Arc::new(Stats::default());
        let (stop, stop_rx) = oneshot::channel::<()>();
        let mut opts = ServerOptions::forward(server_tls, "127.0.0.1:9");
        opts.drain_timeout = Duration::from_secs(1);
        let task = tokio::spawn(run_server(l, opts, stats.clone(), async {
            let _ = stop_rx.await;
        }));
        let cfg =
            rb_tls::client_config(rb_tls::ClientOptions { trust: rb_tls::Trust::Custom(ss.identity.certs.clone()), ..Default::default() })
                .expect("cfg");
        let tcp = TcpStream::connect(addr).await.expect("connect");
        let name = rb_tls::server_name("localhost").expect("name");
        let mut tls = TlsConnector::from(cfg).connect(name, tcp).await.expect("client side of handshake completes");
        // TLS 1.3: the server's rejection arrives on first read.
        let mut buf = [0u8; 1];
        let r = tls.read(&mut buf).await;
        assert!(r.is_err() || r.is_ok_and(|n| n == 0));
        for _ in 0..100 {
            if stats.snapshot().failed == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(stats.snapshot().failed, 1);
        let _ = stop.send(());
        let _ = task.await;
    }

    #[test]
    fn stats_feed_prometheus_families() {
        let m = Arc::new(Metrics::new("rb", "0.0.0"));
        let s = Stats::with_metrics(m.clone(), "server");
        s.record_accept();
        s.record_stream();
        s.record_stream();
        s.record_denied();
        s.record_bytes(5, 7);
        s.record_failure();
        s.record_idle_close();
        s.record_close();
        let text = m.render();
        for want in [
            r#"rb_tunnel_connections_total{side="server"} 1"#,
            r#"rb_tunnel_connections_active{side="server"} 0"#,
            r#"rb_tunnel_streams_total{side="server"} 2"#,
            r#"rb_tunnel_denied_total{side="server"} 1"#,
            r#"rb_tunnel_failures_total{side="server"} 1"#,
            r#"rb_tunnel_idle_closed_total{side="server"} 1"#,
            r#"rb_tunnel_bytes_total{side="server",direction="up"} 5"#,
            r#"rb_tunnel_bytes_total{side="server",direction="down"} 7"#,
        ] {
            assert!(text.contains(want), "missing {want} in\n{text}");
        }
        assert_eq!(s.snapshot().streams, 2);
    }

    #[test]
    fn host_of_strips_port_and_brackets() {
        assert_eq!(host_of("example.com:443"), "example.com");
        assert_eq!(host_of("[::1]:8443"), "::1");
        assert_eq!(host_of("localhost"), "localhost");
    }
}
