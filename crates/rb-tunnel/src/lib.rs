//! TLS tunnelling, stunnel/ghostunnel style.
//!
//! ```text
//!  app ──tcp──▶ [client: listen 127.0.0.1:5432] ══tls══▶ [server: listen :8443] ──tcp──▶ upstream
//! ```
//!
//! [`run_server`] terminates TLS (optionally requiring client certificates) and forwards each
//! connection to a fixed upstream. [`run_client`] accepts plaintext locally and forwards over TLS
//! to a tunnel server. Both stop accepting when `shutdown` resolves and let open connections
//! drain; [`Stats`] counts connections and bytes in each direction.

use rb_tls::rustls::{ClientConfig, ServerConfig};
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_rustls::{TlsAcceptor, TlsConnector};

#[derive(Debug, Default)]
pub struct Stats {
    pub accepted: AtomicU64,
    pub active: AtomicU64,
    pub failed: AtomicU64,
    /// Bytes from the accepting side towards the far side.
    pub bytes_up: AtomicU64,
    /// Bytes from the far side back to the accepting side.
    pub bytes_down: AtomicU64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub accepted: u64,
    pub active: u64,
    pub failed: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

impl Stats {
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            accepted: self.accepted.load(Ordering::Relaxed),
            active: self.active.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            bytes_up: self.bytes_up.load(Ordering::Relaxed),
            bytes_down: self.bytes_down.load(Ordering::Relaxed),
        }
    }
}

pub struct ServerOptions {
    pub tls: Arc<ServerConfig>,
    pub upstream: String,
    pub handshake_timeout: Duration,
    pub drain_timeout: Duration,
}

pub struct ClientOptions {
    pub tls: Arc<ClientConfig>,
    /// Tunnel server address, `host:port`.
    pub remote: String,
    /// Name to verify the tunnel server's certificate against (defaults to the remote host).
    pub server_name: Option<String>,
    pub drain_timeout: Duration,
}

/// Accepts TLS on `listener` and forwards plaintext to `opts.upstream`.
pub async fn run_server(
    listener: TcpListener,
    opts: ServerOptions,
    stats: Arc<Stats>,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let acceptor = TlsAcceptor::from(opts.tls);
    let upstream: Arc<str> = opts.upstream.into();
    let handshake_timeout = opts.handshake_timeout;
    accept_loop(listener, stats, shutdown, opts.drain_timeout, move |inbound, peer, stats| {
        let acceptor = acceptor.clone();
        let upstream = upstream.clone();
        async move {
            let tls = match tokio::time::timeout(handshake_timeout, acceptor.accept(inbound)).await {
                Ok(Ok(tls)) => tls,
                Ok(Err(e)) => return Err(format!("tls handshake from {peer}: {e}")),
                Err(_) => return Err(format!("tls handshake from {peer} timed out")),
            };
            let out = TcpStream::connect(&*upstream).await.map_err(|e| format!("upstream {upstream}: {e}"))?;
            let _ = out.set_nodelay(true);
            pipe(tls, out, &stats).await
        }
    })
    .await
}

/// Accepts plaintext on `listener` and forwards it over TLS to `opts.remote`.
pub async fn run_client(
    listener: TcpListener,
    opts: ClientOptions,
    stats: Arc<Stats>,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let connector = TlsConnector::from(opts.tls);
    let host = opts.server_name.unwrap_or_else(|| host_of(&opts.remote).to_string());
    let name = rb_tls::server_name(&host).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    let remote: Arc<str> = opts.remote.into();
    accept_loop(listener, stats, shutdown, opts.drain_timeout, move |inbound, _peer, stats| {
        let (connector, name, remote) = (connector.clone(), name.clone(), remote.clone());
        async move {
            let tcp = TcpStream::connect(&*remote).await.map_err(|e| format!("tunnel server {remote}: {e}"))?;
            let _ = tcp.set_nodelay(true);
            let tls = connector.connect(name, tcp).await.map_err(|e| format!("tls to {remote}: {e}"))?;
            pipe(inbound, tls, &stats).await
        }
    })
    .await
}

fn host_of(addr: &str) -> &str {
    match addr.rsplit_once(':') {
        Some((host, _)) => host.trim_start_matches('[').trim_end_matches(']'),
        None => addr,
    }
}

async fn accept_loop<F, Fut>(
    listener: TcpListener,
    stats: Arc<Stats>,
    shutdown: impl Future<Output = ()>,
    drain: Duration,
    handle: F,
) -> std::io::Result<()>
where
    F: Fn(TcpStream, SocketAddr, Arc<Stats>) -> Fut,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    let mut shutdown = std::pin::pin!(shutdown);
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
                stats.accepted.fetch_add(1, Ordering::Relaxed);
                stats.active.fetch_add(1, Ordering::Relaxed);
                let fut = handle(tcp, peer, stats.clone());
                let stats = stats.clone();
                conns.spawn(async move {
                    if let Err(e) = fut.await {
                        stats.failed.fetch_add(1, Ordering::Relaxed);
                        tracing::debug!(%peer, error = %e, "tunnel connection failed");
                    }
                    stats.active.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
            () = &mut shutdown => break,
        }
    }
    drop(listener);
    let drained = tokio::time::timeout(drain, async { while conns.join_next().await.is_some() {} }).await;
    if drained.is_err() {
        tracing::warn!(remaining = conns.len(), "drain timeout; closing tunnel connections");
        conns.abort_all();
    }
    Ok(())
}

async fn pipe<A, B>(mut a: A, mut b: B, stats: &Stats) -> Result<(), String>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    match tokio::io::copy_bidirectional(&mut a, &mut b).await {
        Ok((up, down)) => {
            stats.bytes_up.fetch_add(up, Ordering::Relaxed);
            stats.bytes_down.fetch_add(down, Ordering::Relaxed);
            Ok(())
        }
        // Peers that drop without close_notify are routine for tunnels; not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bytes_round_trip_through_client_and_server_with_mtls() {
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
        let client_l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let client_addr = client_l.local_addr().expect("addr");
        let (stop_s, stop_s_rx) = oneshot::channel::<()>();
        let (stop_c, stop_c_rx) = oneshot::channel::<()>();
        let server_stats = Arc::new(Stats::default());
        let client_stats = Arc::new(Stats::default());

        let s = tokio::spawn(run_server(
            server_l,
            ServerOptions {
                tls: server_tls,
                upstream: upstream.to_string(),
                handshake_timeout: Duration::from_secs(5),
                drain_timeout: Duration::from_secs(2),
            },
            server_stats.clone(),
            async {
                let _ = stop_s_rx.await;
            },
        ));
        let c = tokio::spawn(run_client(
            client_l,
            ClientOptions {
                tls: client_tls,
                remote: server_addr.to_string(),
                server_name: Some("localhost".into()),
                drain_timeout: Duration::from_secs(2),
            },
            client_stats.clone(),
            async {
                let _ = stop_c_rx.await;
            },
        ));

        let mut conn = TcpStream::connect(client_addr).await.expect("connect");
        conn.write_all(b"hello through tls").await.expect("write");
        let mut buf = [0u8; 17];
        conn.read_exact(&mut buf).await.expect("read");
        assert_eq!(&buf, b"hello through tls");
        drop(conn);

        // Let both ends see EOF and record their byte counts.
        for _ in 0..100 {
            if client_stats.snapshot().active == 0 && server_stats.snapshot().active == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = stop_c.send(());
        let _ = stop_s.send(());
        let _ = c.await;
        let _ = s.await;
        let cs = client_stats.snapshot();
        assert_eq!((cs.accepted, cs.failed, cs.bytes_up, cs.bytes_down), (1, 0, 17, 17));
        assert_eq!(server_stats.snapshot().accepted, 1);
    }

    #[tokio::test]
    async fn server_rejects_clients_without_a_certificate() {
        let ss = rb_tls::self_signed(&["localhost".into()]).expect("cert");
        let server_tls = rb_tls::server_config_mtls(ss.identity.clone(), &ss.identity.certs, &[]).expect("server cfg");
        let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = l.local_addr().expect("addr");
        let stats = Arc::new(Stats::default());
        let (stop, stop_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(run_server(
            l,
            ServerOptions {
                tls: server_tls,
                upstream: "127.0.0.1:9".into(),
                handshake_timeout: Duration::from_secs(5),
                drain_timeout: Duration::from_secs(1),
            },
            stats.clone(),
            async {
                let _ = stop_rx.await;
            },
        ));
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
    fn host_of_strips_port_and_brackets() {
        assert_eq!(host_of("example.com:443"), "example.com");
        assert_eq!(host_of("[::1]:8443"), "::1");
        assert_eq!(host_of("localhost"), "localhost");
    }
}
