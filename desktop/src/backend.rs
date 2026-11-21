//! Everything that touches the network, on a small tokio runtime shared by the app. The UI
//! thread never awaits: calls here run on Rune's `Latest` workers (which block on this runtime)
//! or spawn onto it.

use rb_client::loadgen::{self, HttpTarget, Run};
use rb_client::{ClientOptions, HttpClient};
use serde::Deserialize;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

pub fn rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rb-desktop-io")
            .enable_all()
            .build()
            .unwrap_or_else(|e| panic!("tokio runtime: {e}"))
    })
}

fn client_options() -> ClientOptions {
    let trust = if std::env::var_os("RB_INSECURE").is_some() { rb_tls::Trust::Insecure } else { rb_tls::Trust::WebPki };
    ClientOptions { tls: rb_tls::ClientOptions { trust, ..Default::default() }, timeout: Duration::from_secs(5), ..Default::default() }
}

fn client() -> Result<&'static HttpClient, String> {
    static CLIENT: OnceLock<Result<HttpClient, String>> = OnceLock::new();
    CLIENT.get_or_init(|| HttpClient::new(client_options()).map_err(|e| e.to_string())).as_ref().map_err(Clone::clone)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Health {
    Unknown,
    Up { latency: Duration, http_version: String },
    Down(String),
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrafficRow {
    pub method: String,
    pub path: String,
    pub count: u64,
    pub status2xx: u64,
    pub status4xx: u64,
    pub status5xx: u64,
    pub total_nanos: u64,
}

impl TrafficRow {
    pub fn avg_ms(&self) -> f64 {
        if self.count == 0 { 0.0 } else { self.total_nanos as f64 / self.count as f64 / 1e6 }
    }
}

#[derive(Clone, Debug)]
pub struct Fetched {
    pub health: Health,
    pub traffic: Option<Vec<TrafficRow>>,
}

/// One refresh: health (with protocol) and the traffic table. Blocking; call from a worker.
pub fn fetch(base: &str) -> Fetched {
    let base = base.trim_end_matches('/').to_string();
    rt().block_on(async move {
        let client = match client() {
            Ok(c) => c,
            Err(e) => return Fetched { health: Health::Down(e), traffic: None },
        };
        let start = Instant::now();
        let health = match client.get(&format!("{base}/api/info")).await {
            Ok(r) if r.status.is_success() => {
                let v: serde_json::Value = r.json().unwrap_or_default();
                let http_version = v["httpVersion"].as_str().unwrap_or("?").to_string();
                Health::Up { latency: start.elapsed(), http_version }
            }
            Ok(r) => Health::Down(format!("HTTP {}", r.status)),
            Err(e) => Health::Down(e.to_string()),
        };
        let traffic = match health {
            Health::Up { .. } => client.get_json::<Vec<TrafficRow>>(&format!("{base}/api/admin/traffic")).await.ok(),
            _ => None,
        };
        Fetched { health, traffic }
    })
}

/// WebSocket round trip to `{base}/ws`. Blocking; call from a worker.
pub fn ws_ping(base: &str) -> Result<Duration, String> {
    let url = base.trim_end_matches('/').replacen("http", "ws", 1) + "/ws";
    rt().block_on(async move {
        let mut ws = rb_client::ws::connect(&url, client_options().tls).await.map_err(|e| e.to_string())?;
        let (_, rtt) = rb_client::ws::round_trip(&mut ws, "ping").await.map_err(|e| e.to_string())?;
        let _ = ws.close(None).await;
        Ok(rtt)
    })
}

/// Starts a 5 s, 16-worker load test against `{base}/healthz` on the shared runtime.
pub fn start_load(base: &str) -> Result<Arc<Run>, String> {
    let client = client()?.clone();
    let url = format!("{}/healthz", base.trim_end_matches('/'));
    let _guard = rt().enter();
    Ok(loadgen::start(
        Arc::new(HttpTarget { client, url }),
        loadgen::Config { concurrency: 16, duration: Duration::from_secs(5), max_requests: None },
    ))
}

/// An in-process rb server on an ephemeral port; stops when dropped.
pub struct Embedded {
    pub url: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for Embedded {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

pub fn start_embedded() -> Result<Embedded, String> {
    let listener = rt().block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let state = rb_server::AppState::new(rb_server::Config::default());
    let app = rb_server::router(state);
    let (stop, rx) = tokio::sync::oneshot::channel::<()>();
    rt().spawn(rb_server::serve(listener, app, rb_server::ServeOptions::default(), async {
        let _ = rx.await;
    }));
    Ok(Embedded { url: format!("http://{addr}"), stop: Some(stop) })
}
