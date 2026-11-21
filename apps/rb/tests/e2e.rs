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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_through_a_tls_tunnel() {
    let srv = Server::start(None).await;
    let ss = rb_tls::self_signed(&["localhost".into()]).expect("cert");

    let tunnel_server_l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let tunnel_server_addr = tunnel_server_l.local_addr().expect("addr");
    let tunnel_client_l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let local = tunnel_client_l.local_addr().expect("addr");
    let (stop_s, rx_s) = oneshot::channel::<()>();
    let (stop_c, rx_c) = oneshot::channel::<()>();

    let ts = tokio::spawn(rb_tunnel::run_server(
        tunnel_server_l,
        rb_tunnel::ServerOptions {
            tls: rb_tls::server_config(ss.identity.clone(), &[]).expect("cfg"),
            upstream: srv.addr.to_string(),
            handshake_timeout: Duration::from_secs(5),
            drain_timeout: Duration::from_millis(100),
        },
        Arc::default(),
        async {
            let _ = rx_s.await;
        },
    ));
    let tc = tokio::spawn(rb_tunnel::run_client(
        tunnel_client_l,
        rb_tunnel::ClientOptions {
            tls: rb_tls::client_config(rb_tls::ClientOptions {
                trust: rb_tls::Trust::Custom(ss.identity.certs.clone()),
                ..Default::default()
            })
            .expect("cfg"),
            remote: tunnel_server_addr.to_string(),
            server_name: Some("localhost".into()),
            drain_timeout: Duration::from_millis(100),
        },
        Arc::default(),
        async {
            let _ = rx_c.await;
        },
    ));

    // Plain HTTP into the local end comes out of the far end at the server.
    let c = client(ClientOptions::default());
    let resp = c.get(&format!("http://{local}/healthz")).await.expect("through tunnel");
    assert_eq!(resp.status, 200);
    let mut ws = rb_client::ws::connect(&format!("ws://{local}/ws"), rb_tls::ClientOptions::default()).await.expect("ws via tunnel");
    assert_eq!(rb_client::ws::round_trip(&mut ws, "tunnelled").await.expect("echo").0, "tunnelled");
    let _ = ws.close(None).await;
    drop(c);

    let _ = stop_c.send(());
    let _ = stop_s.send(());
    let _ = tc.await;
    let _ = ts.await;
    srv.shutdown().await;
}
