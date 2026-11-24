//! End-to-end: real server on an ephemeral port, real client, real TLS. One test binary for the
//! whole suite (each `tests/*.rs` file links its own binary, so they're kept together).

use rb_client::{ClientOptions, HttpClient};
use rb_core::paginator::Page;
use rb_server::{AppState, Config, ServeOptions, User};
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

struct Server {
    addr: SocketAddr,
    state: AppState,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Server {
    async fn start(tls: Option<Arc<rb_tls::rustls::ServerConfig>>) -> Server {
        let state = AppState::new(Config::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (stop, rx) = oneshot::channel::<()>();
        let app = rb_server::router(state.clone());
        let opts = ServeOptions { tls, shutdown_timeout: Duration::from_secs(2), ..Default::default() };
        let task = tokio::spawn(rb_server::serve(listener, app, opts, async {
            let _ = rx.await;
        }));
        Server { addr, state, stop: Some(stop), task }
    }

    fn url(&self, scheme: &str, path: &str) -> String {
        format!("{scheme}://{}{path}", self.addr)
    }

    async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let r = tokio::time::timeout(Duration::from_secs(5), self.task).await;
        assert!(matches!(r, Ok(Ok(Ok(())))), "server shut down cleanly: {r:?}");
    }
}

fn client(opts: ClientOptions) -> HttpClient {
    HttpClient::new(opts).expect("client")
}

fn tls_pair() -> (Arc<rb_tls::rustls::ServerConfig>, rb_tls::ClientOptions) {
    let ss = rb_tls::self_signed(&["localhost".into(), "127.0.0.1".into()]).expect("cert");
    let server = rb_tls::server_config(ss.identity.clone(), &[rb_tls::ALPN_H2, rb_tls::ALPN_HTTP11]).expect("server cfg");
    let client = rb_tls::ClientOptions { trust: rb_tls::Trust::Custom(ss.identity.certs), ..Default::default() };
    (server, client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plaintext_api_h1_and_h2c() {
    let srv = Server::start(None).await;
    let h1 = client(ClientOptions::default());

    let health = h1.get(&srv.url("http", "/healthz")).await.expect("healthz");
    assert_eq!(health.status, 200);
    assert_eq!(health.headers.get("cache-control").map(|v| v.as_bytes()), Some(&b"no-store"[..]));
    assert!(health.headers.contains_key("x-request-id"));
    assert_eq!(health.headers.get("x-content-type-options").map(|v| v.as_bytes()), Some(&b"nosniff"[..]));

    let info: Value = h1.get_json(&srv.url("http", "/api/info")).await.expect("info");
    assert_eq!(info["httpVersion"], "HTTP/1.1");
    assert!(info["peer"].as_str().is_some_and(|p| p.starts_with("127.0.0.1:")));

    let h2c = client(ClientOptions { h2c: true, ..Default::default() });
    let info: Value = h2c.get_json(&srv.url("http", "/api/info")).await.expect("h2c info");
    assert_eq!(info["httpVersion"], "HTTP/2.0");

    // Pagination + caching + invalidation.
    let first = h1.get(&srv.url("http", "/api/users?pageSize=2")).await.expect("users");
    assert_eq!(first.headers.get("x-cache").map(|v| v.as_bytes()), Some(&b"miss"[..]));
    let page: Page<User> = first.json().expect("page");
    assert_eq!(page.items.iter().map(|u| u.id).collect::<Vec<_>>(), vec![1, 2]);
    assert!(page.has_more);
    let again = h1.get(&srv.url("http", "/api/users?pageSize=2")).await.expect("users");
    assert_eq!(again.headers.get("x-cache").map(|v| v.as_bytes()), Some(&b"hit"[..]));
    let cursor = page.next_cursor.expect("cursor");
    let next: Page<User> = h1.get_json(&srv.url("http", &format!("/api/users?pageSize=10&cursor={cursor}"))).await.expect("next");
    assert_eq!(next.items.first().map(|u| u.id), Some(3));
    assert!(!next.has_more);

    let created: User = h1.post_json(&srv.url("http", "/api/users"), &serde_json::json!({"name": "Margaret"})).await.expect("create");
    assert_eq!(created.id, 6);
    let after = h1.get(&srv.url("http", "/api/users?pageSize=2")).await.expect("users");
    assert_eq!(after.headers.get("x-cache").map(|v| v.as_bytes()), Some(&b"miss"[..]), "create invalidated the cache");

    let bad = h1.post_json::<_, Value>(&srv.url("http", "/api/users"), &serde_json::json!({"name": " "})).await;
    assert!(matches!(bad, Err(rb_client::Error::Status { status: 400, .. })));
    let bad_cursor = h1.get(&srv.url("http", "/api/users?cursor=@@")).await.expect("resp");
    assert_eq!(bad_cursor.status, 400);

    let missing = h1.get(&srv.url("http", "/nope")).await.expect("404");
    assert_eq!(missing.status, 404);
    let body: Value = missing.json().expect("json");
    assert_eq!(body["error"]["code"], "not_found");

    // Traffic is keyed by matched route.
    let traffic: Vec<Value> = h1.get_json(&srv.url("http", "/api/admin/traffic")).await.expect("traffic");
    let users_get = traffic.iter().find(|t| t["path"] == "/api/users" && t["method"] == "GET").expect("users route");
    assert_eq!(users_get["count"], 5);
    assert_eq!(users_get["status4xx"], 1);

    let probes: Vec<Value> = h1.get_json(&srv.url("http", "/api/admin/healthchecks")).await.expect("probes");
    assert!(probes.iter().all(|p| p["healthy"] == true), "{probes:?}");

    // Prometheus exposition reflects the same requests, by matched route.
    let _ = rb_client::ws::connect(&srv.url("ws", "/ws"), rb_tls::ClientOptions::default()).await.expect("ws");
    let metrics = h1.get(&srv.url("http", "/metrics")).await.expect("metrics");
    assert_eq!(metrics.status, 200);
    assert!(metrics.headers.get("content-type").is_some_and(|v| v.as_bytes().starts_with(b"application/openmetrics-text")));
    let text = metrics.text();
    assert!(text.contains(r#"rb_http_requests_total{method="GET",route="/api/users",status="200"} 4"#), "{text}");
    assert!(text.contains(r#"rb_http_requests_total{method="GET",route="/api/users",status="400"} 1"#), "{text}");
    assert!(text.contains("rb_http_request_duration_seconds_bucket"), "{text}");
    assert!(text.contains("rb_build_info"), "{text}");

    // Readiness drains before shutdown.
    assert_eq!(h1.get(&srv.url("http", "/readyz")).await.expect("ready").status, 200);
    srv.state.set_shutting_down();
    assert_eq!(h1.get(&srv.url("http", "/readyz")).await.expect("ready").status, 503);
    srv.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tls_negotiates_h2_by_alpn_and_falls_back_to_h1() {
    let (server_tls, client_tls) = tls_pair();
    let srv = Server::start(Some(server_tls)).await;
    let url = format!("https://localhost:{}/api/info", srv.addr.port());

    let auto = client(ClientOptions { tls: client_tls.clone(), ..Default::default() });
    let info: Value = auto.get_json(&url).await.expect("h2");
    assert_eq!(info["httpVersion"], "HTTP/2.0");

    let h1 = client(ClientOptions { tls: client_tls.clone(), http1_only: true, ..Default::default() });
    let info: Value = h1.get_json(&url).await.expect("h1");
    assert_eq!(info["httpVersion"], "HTTP/1.1");

    // Untrusted cert is refused; --insecure accepts it.
    let strict = client(ClientOptions::default());
    assert!(strict.get(&url).await.is_err());
    let insecure =
        client(ClientOptions { tls: rb_tls::ClientOptions { trust: rb_tls::Trust::Insecure, ..Default::default() }, ..Default::default() });
    assert_eq!(insecure.get(&url).await.expect("insecure").status, 200);

    // Tokens over TLS.
    let tok: Value = auto
        .post_json(&format!("https://localhost:{}/api/token", srv.addr.port()), &serde_json::json!({"sub": "ada", "ttlSecs": 60}))
        .await
        .expect("token");
    let bearer = format!("Bearer {}", tok["token"].as_str().expect("token str"));
    let me = auto
        .send(
            http::Method::GET,
            &format!("https://localhost:{}/api/me", srv.addr.port()),
            &[(http::header::AUTHORIZATION, http::HeaderValue::from_str(&bearer).expect("hv"))],
            None,
        )
        .await
        .expect("me");
    assert_eq!(me.status, 200);
    assert_eq!(me.json::<Value>().expect("json")["sub"], "ada");
    let anon = auto.get(&format!("https://localhost:{}/api/me", srv.addr.port())).await.expect("anon");
    assert_eq!(anon.status, 401);

    srv.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn websocket_echo_over_ws_and_wss() {
    let srv = Server::start(None).await;
    let mut ws = rb_client::ws::connect(&srv.url("ws", "/ws"), rb_tls::ClientOptions::default()).await.expect("ws");
    let (reply, _) = rb_client::ws::round_trip(&mut ws, "hello").await.expect("echo");
    assert_eq!(reply, "hello");
    let _ = ws.close(None).await;
    srv.shutdown().await;

    let (server_tls, client_tls) = tls_pair();
    let srv = Server::start(Some(server_tls)).await;
    let mut wss = rb_client::ws::connect(&format!("wss://localhost:{}/ws", srv.addr.port()), client_tls).await.expect("wss");
    for msg in ["one", "two", "three"] {
        assert_eq!(rb_client::ws::round_trip(&mut wss, msg).await.expect("echo").0, msg);
    }
    let _ = wss.close(None).await;
    srv.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loadgen_against_a_live_server() {
    let srv = Server::start(None).await;
    for (name, opts) in [("h1", ClientOptions::default()), ("h2c", ClientOptions { h2c: true, ..Default::default() })] {
        let target = rb_client::loadgen::HttpTarget { client: client(opts), url: srv.url("http", "/healthz") };
        let run = rb_client::loadgen::start(
            Arc::new(target),
            rb_client::loadgen::Config { concurrency: 8, duration: Duration::from_secs(10), max_requests: Some(400) },
        );
        let snap = tokio::time::timeout(Duration::from_secs(10), run.wait()).await.expect("finishes");
        assert_eq!(snap.total, 400, "{name}");
        assert_eq!(snap.statuses.get(&200), Some(&400), "{name}: {snap:?}");
        assert_eq!(snap.errors, 0, "{name}");
    }
    srv.shutdown().await;
}

/// A tunnel end running on an ephemeral port until `stop`.
struct Running {
    addr: SocketAddr,
    stats: Arc<rb_tunnel::Stats>,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Running {
    async fn stop(self) {
        let _ = self.stop.send(());
        let r = tokio::time::timeout(Duration::from_secs(5), self.task).await;
        assert!(matches!(r, Ok(Ok(Ok(())))), "tunnel end stopped cleanly: {r:?}");
    }

    /// Waits until no connection is open on this end, so byte and stream counts are final.
    async fn settle(&self) {
        for _ in 0..300 {
            if self.stats.snapshot().active == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

async fn tunnel_server(mut opts: rb_tunnel::ServerOptions, stats: rb_tunnel::Stats) -> Running {
    let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = l.local_addr().expect("addr");
    let (stop, rx) = oneshot::channel::<()>();
    let stats = Arc::new(stats);
    opts.drain_timeout = Duration::from_millis(200);
    let task = tokio::spawn(rb_tunnel::run_server(l, opts, stats.clone(), async {
        let _ = rx.await;
    }));
    Running { addr, stats, stop, task }
}

async fn tunnel_client(mut opts: rb_tunnel::ClientOptions) -> Running {
    let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = l.local_addr().expect("addr");
    let (stop, rx) = oneshot::channel::<()>();
    let stats = Arc::new(rb_tunnel::Stats::default());
    opts.drain_timeout = Duration::from_millis(200);
    let task = tokio::spawn(rb_tunnel::run_client(l, opts, stats.clone(), async {
        let _ = rx.await;
    }));
    Running { addr, stats, stop, task }
}

/// A self-signed CA-less identity, the server config (optionally requiring that same certificate
/// from clients: mTLS) and client TLS options (presenting it when `mtls`).
fn tunnel_tls(names: &[&str], mtls: bool) -> (Arc<rb_tls::rustls::ServerConfig>, rb_tls::ClientOptions) {
    let ss = rb_tls::self_signed(&names.iter().map(|n| n.to_string()).collect::<Vec<_>>()).expect("cert");
    let server = if mtls {
        rb_tls::server_config_mtls(ss.identity.clone(), &ss.identity.certs, &[]).expect("cfg")
    } else {
        rb_tls::server_config(ss.identity.clone(), &[]).expect("cfg")
    };
    let identity = mtls.then(|| ss.identity.clone());
    (server, rb_tls::ClientOptions { trust: rb_tls::Trust::Custom(ss.identity.certs), identity, ..Default::default() })
}

fn client_opts(tls: &rb_tls::ClientOptions, remote: SocketAddr, mode: rb_tunnel::Mode, target: Option<String>) -> rb_tunnel::ClientOptions {
    let mut o = rb_tunnel::ClientOptions::new(rb_tls::client_config(tls.clone()).expect("client cfg"), remote.to_string());
    o.server_name = Some("localhost".into());
    o.mode = mode;
    o.target = target;
    o
}

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

async fn echo_through(local: SocketAddr, msg: &[u8]) -> std::io::Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut conn = tokio::net::TcpStream::connect(local).await?;
    conn.write_all(msg).await?;
    let mut buf = vec![0u8; msg.len()];
    conn.read_exact(&mut buf).await?;
    Ok(buf)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_through_a_tls_tunnel_in_raw_and_multiplexed_modes_with_mtls() {
    let srv = Server::start(None).await;
    let (server_tls, client_tls) = tunnel_tls(&["localhost"], true);
    let ts = tunnel_server(rb_tunnel::ServerOptions::forward(server_tls, srv.addr.to_string()), rb_tunnel::Stats::default()).await;

    for (mode, target) in [(rb_tunnel::Mode::Raw, None), (rb_tunnel::Mode::H2, Some(srv.addr.to_string()))] {
        let tc = tunnel_client(client_opts(&client_tls, ts.addr, mode, target)).await;
        let local = tc.addr;
        // Plain HTTP into the local end comes out of the far end at the server: several at once,
        // each on its own TCP connection (a fresh client per request).
        let gets = (0..8).map(|_| async move { client(ClientOptions::default()).get(&format!("http://{local}/healthz")).await });
        for resp in futures_util::future::join_all(gets).await {
            assert_eq!(resp.expect("through tunnel").status, 200, "{mode:?}");
        }
        let mut ws = rb_client::ws::connect(&format!("ws://{local}/ws"), rb_tls::ClientOptions::default()).await.expect("ws via tunnel");
        assert_eq!(rb_client::ws::round_trip(&mut ws, "tunnelled").await.expect("echo").0, "tunnelled");
        let _ = ws.close(None).await;
        drop(ws);
        tc.settle().await;
        let cs = tc.stats.snapshot();
        assert_eq!((cs.accepted, cs.failed), (9, 0), "{mode:?}: {cs:?}");
        assert_eq!(cs.streams, if mode == rb_tunnel::Mode::H2 { 9 } else { 0 }, "{mode:?}");
        tc.stop().await;
    }
    ts.settle().await;
    let ss = ts.stats.snapshot();
    // Raw: one TLS connection per forwarded connection. Multiplexed: one for all nine streams.
    assert_eq!((ss.accepted, ss.streams, ss.failed), (9 + 1, 9, 0), "{ss:?}");

    // mTLS holds in multiplexed mode too: no client certificate, no streams.
    let (_, mut no_cert) = tunnel_tls(&["localhost"], true);
    no_cert.identity = None;
    let tc = tunnel_client(client_opts(&no_cert, ts.addr, rb_tunnel::Mode::H2, Some(srv.addr.to_string()))).await;
    let r = client(ClientOptions::default()).get(&format!("http://{}/healthz", tc.addr)).await;
    assert!(r.is_err(), "a client without a certificate must not get through: {r:?}");
    tc.settle().await;
    assert_eq!(tc.stats.snapshot().failed, 1);
    tc.stop().await;

    ts.stop().await;
    srv.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multiplexed_clients_fall_back_to_raw_servers_and_targets_are_allowlisted() {
    let echo = echo_upstream().await;
    let other = echo_upstream().await;
    let (server_tls, client_tls) = tunnel_tls(&["localhost"], false);

    // A server that doesn't multiplex: the h2 client falls back and still gets through.
    let mut raw_only = rb_tunnel::ServerOptions::forward(server_tls.clone(), echo.to_string());
    raw_only.multiplex = false;
    let ts = tunnel_server(raw_only, rb_tunnel::Stats::default()).await;
    let opts = client_opts(&client_tls, ts.addr, rb_tunnel::Mode::H2, Some(echo.to_string()));
    let connector = opts.connector().expect("connector");
    let tc = tunnel_client(opts).await;
    assert_eq!(echo_through(tc.addr, b"fallback").await.expect("echo"), b"fallback");
    assert_eq!(echo_through(tc.addr, b"again").await.expect("echo"), b"again");
    let (_stream, multiplexed) = connector.open(Some(&echo.to_string())).await.expect("open");
    assert!(!multiplexed);
    assert_eq!(connector.multiplexed().await, Some(false));
    tc.settle().await;
    assert_eq!(tc.stats.snapshot().streams, 0);
    tc.stop().await;
    ts.stop().await;

    // Allowlist: the upstream and `other` are allowed; any other port is refused with 403.
    let mut opts = rb_tunnel::ServerOptions::forward(server_tls, echo.to_string());
    opts.allow = rb_tunnel::Allowlist::parse(&[format!("127.0.0.0/8:{}", other.port())]).expect("allow");
    let ts = tunnel_server(opts, rb_tunnel::Stats::default()).await;
    let ok = tunnel_client(client_opts(&client_tls, ts.addr, rb_tunnel::Mode::H2, Some(format!("localhost:{}", other.port())))).await;
    assert_eq!(echo_through(ok.addr, b"by cidr after dns").await.expect("echo"), b"by cidr after dns");
    let closed = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let denied_target = closed.local_addr().expect("addr").to_string();
    let denied = tunnel_client(client_opts(&client_tls, ts.addr, rb_tunnel::Mode::H2, Some(denied_target))).await;
    assert!(echo_through(denied.addr, b"nope").await.is_err(), "a denied target closes the local connection");
    denied.settle().await;
    assert_eq!(denied.stats.snapshot().failed, 1);
    // Both counters move before the server answers, so they're final once the client has seen it.
    assert_eq!((ts.stats.snapshot().denied, ts.stats.snapshot().streams), (1, 1));
    ok.stop().await;
    denied.stop().await;
    ts.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sni_routes_pick_upstream_and_certificate_and_metrics_are_served() {
    let srv = Server::start(None).await;
    let echo = echo_upstream().await;
    let ss = rb_tls::self_signed(&["api.test".into(), "echo.test".into()]).expect("cert");
    let own = rb_tls::self_signed(&["own.test".into()]).expect("cert");
    let default_tls = rb_tls::server_config(ss.identity.clone(), &[]).expect("cfg");
    let mut opts = rb_tunnel::ServerOptions::forward(default_tls, "127.0.0.1:9");
    opts.upstream = None;
    opts.routes = vec![
        rb_tunnel::Route { sni: "API.test".into(), upstream: Some(srv.addr.to_string()), tls: None },
        rb_tunnel::Route { sni: "echo.test".into(), upstream: Some(echo.to_string()), tls: None },
        rb_tunnel::Route {
            sni: "own.test".into(),
            upstream: Some(echo.to_string()),
            tls: Some(rb_tls::server_config(own.identity.clone(), &[]).expect("cfg")),
        },
    ];
    let metrics = Arc::new(rb_telemetry::metrics::Metrics::new("rb", "test"));
    let ts = tunnel_server(opts, rb_tunnel::Stats::with_metrics(metrics.clone(), "server")).await;

    let via = |name: &str, certs: &rb_tls::SelfSigned| {
        let tls = rb_tls::ClientOptions { trust: rb_tls::Trust::Custom(certs.identity.certs.clone()), ..Default::default() };
        let mut o = client_opts(&tls, ts.addr, rb_tunnel::Mode::Raw, None);
        o.server_name = Some(name.to_string());
        o
    };
    let api = tunnel_client(via("api.test", &ss)).await;
    let resp = client(ClientOptions::default()).get(&format!("http://{}/healthz", api.addr)).await.expect("api via sni");
    assert_eq!(resp.status, 200);
    let echo_c = tunnel_client(via("echo.test", &ss)).await;
    assert_eq!(echo_through(echo_c.addr, b"routed by sni").await.expect("echo"), b"routed by sni");
    // own.test presents its own certificate: a client trusting only that one gets through.
    let own_c = tunnel_client(via("own.test", &own)).await;
    assert_eq!(echo_through(own_c.addr, b"own cert").await.expect("echo"), b"own cert");
    // An unknown name with no default upstream is refused.
    let lost = tunnel_client(via("unknown.test", &ss)).await;
    assert!(echo_through(lost.addr, b"x").await.is_err());

    for c in [api, echo_c, own_c, lost] {
        c.settle().await;
        c.stop().await;
    }
    ts.settle().await;

    // The families `rb tunnel --metrics-addr` serves, through the same router.
    let ml = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let maddr = ml.local_addr().expect("addr");
    let (mstop, mrx) = oneshot::channel::<()>();
    let mtask = tokio::spawn(rb_server::serve(ml, rb_server::metrics_router(metrics), ServeOptions::default(), async {
        let _ = mrx.await;
    }));
    let text = client(ClientOptions::default()).get(&format!("http://{maddr}/metrics")).await.expect("metrics");
    assert_eq!(text.status, 200);
    let body = String::from_utf8(text.body.to_vec()).expect("utf8");
    assert!(body.contains(r#"rb_tunnel_connections_total{side="server"} 4"#), "{body}");
    assert!(body.contains(r#"rb_tunnel_failures_total{side="server"} 1"#), "{body}");
    assert!(body.contains(r#"rb_tunnel_connections_active{side="server"} 0"#), "{body}");
    assert!(body.contains(r#"rb_tunnel_bytes_total{side="server",direction="up"}"#), "{body}");
    let missing = client(ClientOptions::default()).get(&format!("http://{maddr}/nope")).await.expect("404");
    assert_eq!(missing.status, 404);
    let _ = mstop.send(());
    let _ = mtask.await;

    ts.stop().await;
    srv.shutdown().await;
}
