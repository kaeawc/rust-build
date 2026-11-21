use axum::Router;
use axum::extract::ConnectInfo;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::server::graceful::GracefulShutdown;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;

pub struct ServeOptions {
    /// Serve TLS with this config (its ALPN list decides h2 vs http/1.1). `None` serves plaintext
    /// HTTP/1.1 plus h2c with prior knowledge.
    pub tls: Option<Arc<rb_tls::rustls::ServerConfig>>,
    /// How long in-flight connections get to finish after shutdown starts.
    pub shutdown_timeout: Duration,
    pub tls_handshake_timeout: Duration,
}

impl Default for ServeOptions {
    fn default() -> Self {
        ServeOptions { tls: None, shutdown_timeout: Duration::from_secs(10), tls_handshake_timeout: Duration::from_secs(10) }
    }
}

/// Serves `app` on `listener` until `shutdown` resolves, then drains connections for at most
/// `shutdown_timeout`.
pub async fn serve(listener: TcpListener, app: Router, opts: ServeOptions, shutdown: impl Future<Output = ()>) -> std::io::Result<()> {
    let mut builder = Builder::new(TokioExecutor::new());
    builder.http1().timer(hyper_util::rt::TokioTimer::new()).header_read_timeout(Duration::from_secs(10));
    // RFC 8441: lets `WebSocketUpgrade` accept WebSockets over HTTP/2.
    builder.http2().timer(hyper_util::rt::TokioTimer::new()).enable_connect_protocol();
    let acceptor = opts.tls.map(TlsAcceptor::from);
    let graceful = GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown);

    loop {
        let (tcp, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(conn) => conn,
                Err(e) => {
                    // EMFILE and friends: back off instead of spinning.
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let _ = tcp.set_nodelay(true);
        let app = app.clone();
        let builder = builder.clone();
        let acceptor = acceptor.clone();
        let watcher = graceful.watcher();
        let handshake_timeout = opts.tls_handshake_timeout;
        tokio::spawn(async move {
            let svc = hyper::service::service_fn(move |mut req: hyper::Request<Incoming>| {
                req.extensions_mut().insert(ConnectInfo(peer));
                app.clone().oneshot(req)
            });
            let result = match acceptor {
                Some(acceptor) => match tokio::time::timeout(handshake_timeout, acceptor.accept(tcp)).await {
                    Ok(Ok(tls)) => watcher.watch(builder.serve_connection_with_upgrades(TokioIo::new(tls), svc).into_owned()).await,
                    Ok(Err(e)) => {
                        tracing::debug!(%peer, error = %e, "tls handshake failed");
                        return;
                    }
                    Err(_) => {
                        tracing::debug!(%peer, "tls handshake timed out");
                        return;
                    }
                },
                None => watcher.watch(builder.serve_connection_with_upgrades(TokioIo::new(tcp), svc).into_owned()).await,
            };
            if let Err(e) = result {
                tracing::debug!(%peer, error = %e, "connection ended with error");
            }
        });
    }

    drop(listener);
    tokio::select! {
        () = graceful.shutdown() => tracing::info!("all connections drained"),
        () = tokio::time::sleep(opts.shutdown_timeout) => tracing::warn!("shutdown timeout; dropping remaining connections"),
    }
    Ok(())
}

/// Resolves on Ctrl-C or SIGTERM.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
}
