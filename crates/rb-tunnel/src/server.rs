//! The TLS-terminating side: SNI routing, raw forwarding, and HTTP/2 stream multiplexing.

use crate::allow::{Allowlist, Denied};
use crate::target::{PROTOCOL, parse_path, split_target};
use crate::{Stats, accept_loop, io::pipe};
use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use http_body_util::Empty;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use rb_tls::rustls::ServerConfig;
use rb_tls::rustls::server::Acceptor;
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_rustls::LazyConfigAcceptor;

/// One SNI name served with its own upstream and/or certificate.
#[derive(Clone)]
pub struct Route {
    /// Server name the client sends (matched case-insensitively).
    pub sni: String,
    /// Raw-mode upstream for this name; also always allowed as a multiplexed target.
    pub upstream: Option<String>,
    /// Certificate for this name; the default config's otherwise. Its client-auth settings are
    /// used as-is, so build it with the same client CAs to keep mTLS on every route.
    pub tls: Option<Arc<ServerConfig>>,
}

pub struct ServerOptions {
    /// Default certificate (and client-auth policy). ALPN is set by the server.
    pub tls: Arc<ServerConfig>,
    /// Raw-mode upstream when no route matches; also always allowed as a multiplexed target.
    pub upstream: Option<String>,
    pub routes: Vec<Route>,
    /// Targets multiplexed streams may name, on top of the matching upstream.
    pub allow: Allowlist,
    /// Advertise ALPN `h2` and accept multiplexed connections.
    pub multiplex: bool,
    /// Deadline for the whole TLS handshake (ClientHello included).
    pub handshake_timeout: Duration,
    pub connect_timeout: Duration,
    pub drain_timeout: Duration,
    /// Close a relayed connection or stream after this long with no bytes either way.
    pub idle_timeout: Option<Duration>,
    /// HTTP/2 PING interval on multiplexed connections (the peer must answer within it).
    pub keepalive: Option<Duration>,
}

impl ServerOptions {
    /// Forwards everything to `upstream`, multiplexing allowed, 10 s timeouts.
    pub fn forward(tls: Arc<ServerConfig>, upstream: impl Into<String>) -> Self {
        ServerOptions {
            tls,
            upstream: Some(upstream.into()),
            routes: Vec::new(),
            allow: Allowlist::default(),
            multiplex: true,
            handshake_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(10),
            drain_timeout: Duration::from_secs(10),
            idle_timeout: None,
            keepalive: None,
        }
    }
}

/// What a connection resolved to after the ClientHello.
struct Endpoint {
    tls: Arc<ServerConfig>,
    upstream: Option<Arc<str>>,
    allow: Arc<Allowlist>,
}

struct Shared {
    default: Endpoint,
    routes: HashMap<String, Endpoint>,
    handshake_timeout: Duration,
    connect_timeout: Duration,
    idle_timeout: Option<Duration>,
    keepalive: Option<Duration>,
}

fn invalid(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())
}

fn endpoint(tls: &Arc<ServerConfig>, upstream: Option<String>, allow: &Allowlist, multiplex: bool) -> std::io::Result<Endpoint> {
    let mut cfg = (**tls).clone();
    cfg.alpn_protocols = if multiplex { vec![rb_tls::ALPN_H2.to_vec()] } else { Vec::new() };
    let allow = match &upstream {
        Some(u) => allow.clone().with_exact(u).map_err(invalid)?,
        None => allow.clone(),
    };
    Ok(Endpoint { tls: Arc::new(cfg), upstream: upstream.map(Arc::from), allow: Arc::new(allow) })
}

/// Accepts TLS on `listener`. Raw connections go to the route's or default upstream; h2
/// connections carry one CONNECT stream per tunnelled TCP connection, checked against the
/// allowlist.
pub async fn run_server(
    listener: tokio::net::TcpListener,
    opts: ServerOptions,
    stats: Arc<Stats>,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let mut routes = HashMap::new();
    for r in opts.routes {
        let tls = r.tls.as_ref().unwrap_or(&opts.tls);
        routes.insert(r.sni.to_ascii_lowercase(), endpoint(tls, r.upstream, &opts.allow, opts.multiplex)?);
    }
    let shared = Arc::new(Shared {
        default: endpoint(&opts.tls, opts.upstream, &opts.allow, opts.multiplex)?,
        routes,
        handshake_timeout: opts.handshake_timeout,
        connect_timeout: opts.connect_timeout,
        idle_timeout: opts.idle_timeout,
        keepalive: opts.keepalive,
    });
    accept_loop(listener, stats, shutdown, opts.drain_timeout, move |inbound, peer, stats, draining| {
        let shared = shared.clone();
        async move { connection(inbound, peer, shared, stats, draining).await }
    })
    .await
}

async fn connection(
    inbound: TcpStream,
    peer: std::net::SocketAddr,
    shared: Arc<Shared>,
    stats: Arc<Stats>,
    draining: watch::Receiver<bool>,
) -> Result<(), String> {
    // One deadline covers reading the ClientHello and finishing the handshake.
    let handshake = async {
        let start = LazyConfigAcceptor::new(Acceptor::default(), inbound).await.map_err(|e| format!("client hello from {peer}: {e}"))?;
        let sni = start.client_hello().server_name().map(str::to_ascii_lowercase);
        let ep = sni.as_deref().and_then(|s| shared.routes.get(s)).unwrap_or(&shared.default);
        let tls = start.into_stream(ep.tls.clone()).await.map_err(|e| format!("tls handshake from {peer}: {e}"))?;
        Ok::<_, String>((tls, ep, sni))
    };
    let (tls, ep, sni) =
        tokio::time::timeout(shared.handshake_timeout, handshake).await.map_err(|_| format!("tls handshake from {peer} timed out"))??;
    let multiplexed = tls.get_ref().1.alpn_protocol() == Some(rb_tls::ALPN_H2);
    tracing::debug!(%peer, ?sni, multiplexed, "tunnel connection");
    if multiplexed {
        return serve_h2(tls, ep.allow.clone(), shared.clone(), stats, draining).await;
    }
    let upstream = ep.upstream.as_deref().ok_or_else(|| format!("no upstream for sni {sni:?} (raw mode needs --upstream or a route)"))?;
    let out = tokio::time::timeout(shared.connect_timeout, TcpStream::connect(upstream))
        .await
        .map_err(|_| format!("upstream {upstream}: connect timed out"))?
        .map_err(|e| format!("upstream {upstream}: {e}"))?;
    let _ = out.set_nodelay(true);
    pipe(tls, out, &stats, shared.idle_timeout).await
}

type StreamTask = Pin<Box<dyn Future<Output = ()> + Send>>;

struct StreamCtx {
    allow: Arc<Allowlist>,
    shared: Arc<Shared>,
    stats: Arc<Stats>,
    /// Upgraded streams run on the connection's task set, so aborting the connection on drain
    /// timeout ends them too.
    spawn: mpsc::UnboundedSender<StreamTask>,
}

async fn serve_h2<S>(
    io: S,
    allow: Arc<Allowlist>,
    shared: Arc<Shared>,
    stats: Arc<Stats>,
    mut draining: watch::Receiver<bool>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let mut builder = hyper::server::conn::http2::Builder::new(TokioExecutor::new());
    builder.timer(TokioTimer::new()).enable_connect_protocol();
    if let Some(every) = shared.keepalive {
        builder.keep_alive_interval(every).keep_alive_timeout(every);
    }
    let (spawn, mut spawned) = mpsc::unbounded_channel();
    let ctx = Arc::new(StreamCtx { allow, shared, stats, spawn });
    let svc = hyper::service::service_fn(move |req| {
        let ctx = ctx.clone();
        async move { Ok::<_, Infallible>(stream(req, ctx).await) }
    });
    let conn = builder.serve_connection(TokioIo::new(io), svc);
    let mut conn = std::pin::pin!(conn);
    let mut streams = JoinSet::new();
    let mut conn_result = None;
    loop {
        tokio::select! {
            r = conn.as_mut(), if conn_result.is_none() => conn_result = Some(r),
            Some(task) = spawned.recv() => { streams.spawn(task); }
            Some(_) = streams.join_next(), if !streams.is_empty() => {}
            Ok(()) = draining.changed(), if conn_result.is_none() => conn.as_mut().graceful_shutdown(),
            else => break,
        }
        if conn_result.is_some() && streams.is_empty() {
            match spawned.try_recv() {
                Ok(task) => {
                    streams.spawn(task);
                }
                Err(_) => break,
            }
        }
    }
    match conn_result {
        Some(Err(e)) if !e.is_canceled() => Err(format!("h2: {e}")),
        _ => Ok(()),
    }
}

fn status(code: StatusCode) -> Response<Empty<Bytes>> {
    let mut r = Response::new(Empty::new());
    *r.status_mut() = code;
    r
}

/// One CONNECT stream: resolve and check the target, dial it, answer 200, then relay.
async fn stream(mut req: Request<Incoming>, ctx: Arc<StreamCtx>) -> Response<Empty<Bytes>> {
    if req.method() != Method::CONNECT {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    }
    let target = match req.extensions().get::<hyper::ext::Protocol>() {
        Some(p) if p.as_str() == PROTOCOL => parse_path(req.uri().path()),
        Some(_) => return status(StatusCode::NOT_IMPLEMENTED),
        // Classic CONNECT (RFC 9113 §8.5): the authority is the target.
        None => req.uri().authority().and_then(|a| split_target(a.as_str())),
    };
    let Some((host, port)) = target else {
        return status(StatusCode::BAD_REQUEST);
    };
    let addrs = match ctx.allow.resolve(&host, port).await {
        Ok(addrs) => addrs,
        Err(Denied::NotAllowed) => {
            ctx.stats.record_denied();
            tracing::info!(%host, port, "tunnel target denied");
            return status(StatusCode::FORBIDDEN);
        }
        Err(e @ Denied::Resolve(_)) => {
            ctx.stats.record_failure();
            tracing::debug!(%host, port, error = %e, "tunnel target unresolved");
            return status(StatusCode::BAD_GATEWAY);
        }
    };
    let out = match tokio::time::timeout(ctx.shared.connect_timeout, TcpStream::connect(&addrs[..])).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            ctx.stats.record_failure();
            tracing::debug!(%host, port, error = %e, "tunnel target unreachable");
            return status(StatusCode::BAD_GATEWAY);
        }
        Err(_) => {
            ctx.stats.record_failure();
            return status(StatusCode::GATEWAY_TIMEOUT);
        }
    };
    let _ = out.set_nodelay(true);
    ctx.stats.record_stream();
    let upgrade = hyper::upgrade::on(&mut req);
    let (stats, idle) = (ctx.stats.clone(), ctx.shared.idle_timeout);
    let task: StreamTask = Box::pin(async move {
        match upgrade.await {
            Ok(up) => {
                if let Err(e) = pipe(TokioIo::new(up), out, &stats, idle).await {
                    stats.record_failure();
                    tracing::debug!(error = %e, "tunnel stream failed");
                }
            }
            Err(e) => {
                stats.record_failure();
                tracing::debug!(error = %e, "tunnel stream upgrade failed");
            }
        }
    });
    if ctx.spawn.send(task).is_err() {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    }
    status(StatusCode::OK)
}
