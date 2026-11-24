//! The plaintext-accepting side: opens tunnel streams through a shared [`Connector`].

use crate::io::{BoxStream, pipe};
use crate::target::{PROTOCOL, path, split_target};
use crate::{Stats, accept_loop, host_of};
use bytes::Bytes;
use http::{Method, Request, StatusCode};
use http_body_util::Empty;
use hyper::client::conn::http2::SendRequest;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use rb_tls::pki::ServerName;
use rb_tls::rustls::ClientConfig;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;

/// How the client reaches the tunnel server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Multiplex when a target is set, raw otherwise.
    #[default]
    Auto,
    /// Multiplex over one HTTP/2 connection (falls back to raw if the server can't).
    H2,
    /// One TLS connection per forwarded connection.
    Raw,
}

impl std::str::FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(Mode::Auto),
            "h2" => Ok(Mode::H2),
            "raw" => Ok(Mode::Raw),
            other => Err(format!("unknown mode {other:?} (auto, h2, raw)")),
        }
    }
}

pub struct ConnectorOptions {
    pub tls: Arc<ClientConfig>,
    /// Tunnel server address, `host:port`.
    pub remote: String,
    /// Name to verify the tunnel server's certificate against, and the SNI sent (defaults to the
    /// remote host).
    pub server_name: Option<String>,
    /// Try to multiplex; `false` always uses raw connections.
    pub multiplex: bool,
    pub connect_timeout: Duration,
    /// HTTP/2 PING interval on the shared connection.
    pub keepalive: Option<Duration>,
}

enum Session {
    /// Not connected yet (or the last connection died).
    Unknown,
    H2(SendRequest<Empty<Bytes>>),
    /// The server doesn't multiplex: every stream is its own TLS connection.
    Raw,
}

/// Opens tunnel streams to one server, sharing a single HTTP/2 connection between them when the
/// server supports it. Clone the `Arc` across listeners to share the connection.
pub struct Connector {
    h2_tls: TlsConnector,
    raw_tls: TlsConnector,
    name: ServerName<'static>,
    remote: Arc<str>,
    multiplex: bool,
    connect_timeout: Duration,
    keepalive: Option<Duration>,
    session: tokio::sync::Mutex<Session>,
    warned_fallback: AtomicBool,
}

fn invalid(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())
}

impl Connector {
    pub fn new(opts: ConnectorOptions) -> std::io::Result<Arc<Self>> {
        let host = opts.server_name.unwrap_or_else(|| host_of(&opts.remote).to_string());
        let name = rb_tls::server_name(&host).map_err(invalid)?;
        let with_alpn = |alpn: Vec<Vec<u8>>| {
            let mut cfg = (*opts.tls).clone();
            cfg.alpn_protocols = alpn;
            TlsConnector::from(Arc::new(cfg))
        };
        Ok(Arc::new(Connector {
            h2_tls: with_alpn(vec![rb_tls::ALPN_H2.to_vec()]),
            raw_tls: with_alpn(Vec::new()),
            name,
            remote: opts.remote.into(),
            multiplex: opts.multiplex,
            connect_timeout: opts.connect_timeout,
            keepalive: opts.keepalive,
            session: tokio::sync::Mutex::new(Session::Unknown),
            warned_fallback: AtomicBool::new(false),
        }))
    }

    /// `Some(true)` once multiplexing is in use, `Some(false)` after falling back to raw, `None`
    /// before the first connection.
    pub async fn multiplexed(&self) -> Option<bool> {
        match &*self.session.lock().await {
            Session::Unknown => None,
            Session::H2(_) => Some(true),
            Session::Raw => Some(false),
        }
    }

    async fn tls(&self, tls: &TlsConnector) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
        let remote = &*self.remote;
        let connect = async {
            let tcp = TcpStream::connect(remote).await.map_err(|e| format!("tunnel server {remote}: {e}"))?;
            let _ = tcp.set_nodelay(true);
            tls.connect(self.name.clone(), tcp).await.map_err(|e| format!("tls to {remote}: {e}"))
        };
        tokio::time::timeout(self.connect_timeout, connect).await.map_err(|_| format!("tunnel server {remote}: connect timed out"))?
    }

    async fn raw(&self) -> Result<BoxStream, String> {
        Ok(Box::new(self.tls(&self.raw_tls).await?))
    }

    /// Opens a stream to `target` (`host:port`). Without a target, or when not multiplexing, the
    /// stream is a raw connection and the server picks the upstream.
    pub async fn open(&self, target: Option<&str>) -> Result<(BoxStream, bool), String> {
        let target = match target {
            Some(t) if self.multiplex => Some(split_target(t).ok_or_else(|| format!("bad target {t:?} (want host:port)"))?),
            _ => return Ok((self.raw().await?, false)),
        };
        // One retry: a cached connection can die between the liveness check and the request.
        for attempt in 0..2 {
            let mut session = self.session.lock().await;
            let mut sender = match &*session {
                Session::H2(s) if !s.is_closed() => s.clone(),
                Session::Raw => {
                    drop(session);
                    self.warn_fallback();
                    return Ok((self.raw().await?, false));
                }
                _ => match self.establish().await? {
                    Established::H2(s) => {
                        *session = Session::H2(s.clone());
                        s
                    }
                    Established::Raw(stream) => {
                        *session = Session::Raw;
                        drop(session);
                        self.warn_fallback();
                        return Ok((stream, false));
                    }
                },
            };
            drop(session);
            match self.connect_stream(&mut sender, target.as_ref()).await {
                Ok(s) => return Ok((s, true)),
                Err(StreamError::Refused(e)) => return Err(e),
                Err(StreamError::Connection(e)) if attempt == 0 => {
                    tracing::debug!(error = %e, "multiplexed connection failed; reconnecting");
                    let mut session = self.session.lock().await;
                    if matches!(&*session, Session::H2(s) if s.is_closed()) {
                        *session = Session::Unknown;
                    }
                }
                Err(StreamError::Connection(e)) => return Err(e),
            }
        }
        Err("unreachable: retry loop exhausted".into())
    }

    fn warn_fallback(&self) {
        if !self.warned_fallback.swap(true, Ordering::Relaxed) {
            tracing::warn!(remote = %self.remote, "tunnel server doesn't multiplex; using one TLS connection per stream (targets are chosen by the server)");
        }
    }

    /// Connects with ALPN `h2`. A server that doesn't pick h2 is a raw server, and this very TLS
    /// connection becomes the first raw stream. One that picks h2 but doesn't enable extended
    /// CONNECT gets a fresh raw connection.
    async fn establish(&self) -> Result<Established, String> {
        let tls = self.tls(&self.h2_tls).await?;
        if tls.get_ref().1.alpn_protocol() != Some(rb_tls::ALPN_H2) {
            return Ok(Established::Raw(Box::new(tls)));
        }
        let mut builder = hyper::client::conn::http2::Builder::new(TokioExecutor::new());
        builder.timer(TokioTimer::new());
        if let Some(every) = self.keepalive {
            builder.keep_alive_interval(every).keep_alive_timeout(every).keep_alive_while_idle(true);
        }
        let (mut sender, conn) = builder.handshake(TokioIo::new(tls)).await.map_err(|e| format!("h2 handshake: {e}"))?;
        let mut conn = Box::pin(conn);
        // The server's SETTINGS (with ENABLE_CONNECT_PROTOCOL) precede any response on the
        // connection, so after one round trip we know whether extended CONNECT is on. The
        // connection is driven here until then, and handed to its own task after.
        let probe = Request::builder()
            .method(Method::GET)
            .uri(format!("https://{}/", self.remote))
            .body(Empty::new())
            .map_err(|e| e.to_string())?;
        let probed = async {
            tokio::select! {
                r = sender.send_request(probe) => r.map(drop).map_err(|e| format!("h2 probe: {e}")),
                r = conn.as_mut() => Err(format!("h2 connection closed during probe: {r:?}")),
            }
        };
        tokio::time::timeout(self.connect_timeout, probed).await.map_err(|_| "h2 probe timed out".to_string())??;
        let extended_connect = conn.is_extended_connect_protocol_enabled();
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!(error = %e, "multiplexed tunnel connection closed");
            }
        });
        if extended_connect { Ok(Established::H2(sender)) } else { Ok(Established::Raw(self.raw().await?)) }
    }

    async fn connect_stream(
        &self,
        sender: &mut SendRequest<Empty<Bytes>>,
        target: Option<&(String, u16)>,
    ) -> Result<BoxStream, StreamError> {
        let Some((host, port)) = target else {
            return Err(StreamError::Refused("no target".into()));
        };
        let mut req = Request::builder()
            .method(Method::CONNECT)
            .uri(format!("https://{}{}", self.remote, path(host, *port)))
            .body(Empty::new())
            .map_err(|e| StreamError::Refused(e.to_string()))?;
        req.extensions_mut().insert(hyper::ext::Protocol::from_static(PROTOCOL));
        let resp = sender.send_request(req).await.map_err(|e| StreamError::Connection(format!("open stream: {e}")))?;
        if resp.status() != StatusCode::OK {
            return Err(StreamError::Refused(format!("tunnel server refused {host}:{port}: {}", resp.status())));
        }
        let up = hyper::upgrade::on(resp).await.map_err(|e| StreamError::Connection(format!("stream upgrade: {e}")))?;
        Ok(Box::new(TokioIo::new(up)))
    }
}

enum Established {
    H2(SendRequest<Empty<Bytes>>),
    Raw(BoxStream),
}

enum StreamError {
    /// The server answered and said no; retrying won't help.
    Refused(String),
    Connection(String),
}

pub struct ForwardOptions {
    pub drain_timeout: Duration,
    pub idle_timeout: Option<Duration>,
}

/// Accepts plaintext on `listener` and relays each connection through `connector` to `target`.
pub async fn run_forward(
    listener: TcpListener,
    connector: Arc<Connector>,
    target: Option<String>,
    opts: ForwardOptions,
    stats: Arc<Stats>,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let target: Option<Arc<str>> = target.map(Arc::from);
    let idle = opts.idle_timeout;
    accept_loop(listener, stats, shutdown, opts.drain_timeout, move |inbound, _peer, stats, _draining| {
        let (connector, target) = (connector.clone(), target.clone());
        async move {
            let (out, multiplexed) = connector.open(target.as_deref()).await?;
            if multiplexed {
                stats.record_stream();
            }
            pipe(inbound, out, &stats, idle).await
        }
    })
    .await
}

pub struct ClientOptions {
    pub tls: Arc<ClientConfig>,
    /// Tunnel server address, `host:port`.
    pub remote: String,
    /// Name to verify the tunnel server's certificate against (defaults to the remote host).
    pub server_name: Option<String>,
    /// `host:port` the server should dial for each stream (multiplexed mode).
    pub target: Option<String>,
    pub mode: Mode,
    pub connect_timeout: Duration,
    pub drain_timeout: Duration,
    pub idle_timeout: Option<Duration>,
    pub keepalive: Option<Duration>,
}

impl ClientOptions {
    /// Raw forwarding to `remote` with 10 s timeouts.
    pub fn new(tls: Arc<ClientConfig>, remote: impl Into<String>) -> Self {
        ClientOptions {
            tls,
            remote: remote.into(),
            server_name: None,
            target: None,
            mode: Mode::Auto,
            connect_timeout: Duration::from_secs(10),
            drain_timeout: Duration::from_secs(10),
            idle_timeout: None,
            keepalive: None,
        }
    }

    /// The connector these options describe.
    pub fn connector(&self) -> std::io::Result<Arc<Connector>> {
        let multiplex = match self.mode {
            Mode::Raw => false,
            Mode::H2 => {
                if self.target.is_none() {
                    return Err(invalid("h2 mode needs a target (host:port)"));
                }
                true
            }
            Mode::Auto => self.target.is_some(),
        };
        if let Some(t) = &self.target {
            split_target(t).ok_or_else(|| invalid(format!("bad target {t:?} (want host:port)")))?;
        }
        Connector::new(ConnectorOptions {
            tls: self.tls.clone(),
            remote: self.remote.clone(),
            server_name: self.server_name.clone(),
            multiplex,
            connect_timeout: self.connect_timeout,
            keepalive: self.keepalive,
        })
    }
}

/// Accepts plaintext on `listener` and forwards it over TLS to `opts.remote`.
pub async fn run_client(
    listener: TcpListener,
    opts: ClientOptions,
    stats: Arc<Stats>,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let connector = opts.connector()?;
    let fwd = ForwardOptions { drain_timeout: opts.drain_timeout, idle_timeout: opts.idle_timeout };
    run_forward(listener, connector, opts.target, fwd, stats, shutdown).await
}
