//! rust-build's desktop control panel, built with Rune.
//!
//! Watches a server's health and HTTP version, its per-route traffic, runs a 5 s load test and a
//! WebSocket ping, and can run an embedded server in-process. `RB_URL=http://host:port` points it
//! at an existing server; without it, the app starts an embedded one.
//!
//! Handlers only queue [`Action`]s; `poll` performs them. That keeps every handler pure, so the
//! headless tests below drive the real view without touching the network.
//!
//! `cargo run` opens the window; `cargo run -- --snap panel.png --scene demo` renders headless.

#![forbid(unsafe_code)]

mod backend;
mod view;

use backend::{Embedded, Fetched, Health, TrafficRow};
use rb_client::loadgen::{Run, Snapshot};
use rune::{AppSpec, Latest, Tx, Waker};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ToggleEmbedded,
    StartLoad,
    StopLoad,
    PingWs,
    Refresh,
}

pub struct State {
    pub url: String,
    pub health: Health,
    pub traffic: Vec<TrafficRow>,
    pub load: Option<Snapshot>,
    pub ws_rtt: Option<Result<Duration, String>>,
    pub actions: Vec<Action>,
    /// No network side effects (headless scenes and tests).
    pub offline: bool,
    embedded: Option<Embedded>,
    load_run: Option<Arc<Run>>,
    fetch: Latest<Fetched>,
    ping: Latest<Result<Duration, String>>,
    next_refresh: f64,
    busy: Arc<AtomicBool>,
}

impl State {
    pub fn embedded_running(&self) -> bool {
        self.embedded.is_some()
    }
    pub fn load_running(&self) -> bool {
        self.load_run.as_ref().is_some_and(|r| !r.is_done())
    }
    pub fn ping_pending(&self) -> bool {
        self.ping.is_pending()
    }
}

fn init() -> State {
    let url = std::env::var("RB_URL").ok();
    let mut actions = vec![Action::Refresh];
    if url.is_none() {
        actions.insert(0, Action::ToggleEmbedded);
    }
    State {
        url: url.unwrap_or_else(|| "http://127.0.0.1:8080".into()),
        health: Health::Unknown,
        traffic: Vec::new(),
        load: None,
        ws_rtt: None,
        actions,
        offline: false,
        embedded: None,
        load_run: None,
        fetch: Latest::default(),
        ping: Latest::default(),
        next_refresh: 0.0,
        busy: Arc::new(AtomicBool::new(false)),
    }
}

/// Fixed data for snapshots and tests.
fn demo(s: &mut State) {
    s.offline = true;
    s.actions.clear();
    s.url = "https://localhost:8443".into();
    s.health = Health::Up { latency: Duration::from_micros(1830), http_version: "HTTP/2.0".into() };
    s.ws_rtt = Some(Ok(Duration::from_micros(640)));
    s.traffic = vec![
        TrafficRow {
            method: "GET".into(),
            path: "/api/users".into(),
            count: 1204,
            status2xx: 1190,
            status4xx: 14,
            status5xx: 0,
            total_nanos: 1_204 * 2_100_000,
        },
        TrafficRow {
            method: "POST".into(),
            path: "/api/token".into(),
            count: 87,
            status2xx: 85,
            status4xx: 2,
            status5xx: 0,
            total_nanos: 87 * 900_000,
        },
        TrafficRow {
            method: "GET".into(),
            path: "/api/info".into(),
            count: 342,
            status2xx: 342,
            status4xx: 0,
            status5xx: 0,
            total_nanos: 342 * 400_000,
        },
    ];
    s.load = Some(Snapshot {
        elapsed: Duration::from_secs(5),
        total: 61_234,
        errors: 0,
        statuses: [(200, 61_234)].into_iter().collect(),
        rps: 12_246.8,
        mean: Duration::from_micros(1300),
        p50: Duration::from_micros(1100),
        p95: Duration::from_micros(2900),
        p99: Duration::from_micros(4700),
        done: true,
        last_error: None,
    });
}

fn setup(s: &mut State, waker: Waker) {
    s.fetch.set_waker(waker.clone());
    s.ping.set_waker(waker.clone());
    if s.offline {
        return;
    }
    // Refresh ticks: 4/s while a load test runs, 1/s otherwise. The window idles between them.
    let busy = s.busy.clone();
    std::thread::Builder::new()
        .name("rb-desktop-tick".into())
        .spawn(move || {
            loop {
                std::thread::sleep(if busy.load(Ordering::Relaxed) { Duration::from_millis(250) } else { Duration::from_secs(1) });
                waker();
            }
        })
        .map(drop)
        .unwrap_or_else(|e| eprintln!("tick thread: {e}"));
}

fn poll(s: &mut State, t: f64, _tx: &mut Tx) -> bool {
    let mut changed = false;
    for action in std::mem::take(&mut s.actions) {
        changed = true;
        if s.offline {
            continue;
        }
        match action {
            Action::ToggleEmbedded => {
                if s.embedded.take().is_none() {
                    match backend::start_embedded() {
                        Ok(e) => {
                            s.url = e.url.clone();
                            s.embedded = Some(e);
                        }
                        Err(e) => s.health = Health::Down(format!("embedded server: {e}")),
                    }
                }
                s.traffic.clear();
                s.next_refresh = t;
            }
            Action::StartLoad if !s.load_running() => match backend::start_load(&s.url) {
                Ok(run) => s.load_run = Some(run),
                Err(e) => s.health = Health::Down(e),
            },
            Action::StartLoad => {}
            Action::StopLoad => {
                if let Some(run) = &s.load_run {
                    run.stop();
                }
            }
            Action::PingWs => {
                let url = s.url.clone();
                s.ping.request(url, |u| backend::ws_ping(&u));
            }
            Action::Refresh => s.next_refresh = t,
        }
    }
    if s.offline {
        return changed;
    }
    if t >= s.next_refresh && !s.fetch.is_pending() {
        s.next_refresh = t + 1.0;
        let url = s.url.clone();
        s.fetch.request(url, |u| backend::fetch(&u));
    }
    if let Some(f) = s.fetch.take() {
        s.health = f.health;
        if let Some(t) = f.traffic {
            s.traffic = t;
        }
        changed = true;
    }
    if let Some(r) = s.ping.take() {
        s.ws_rtt = Some(r);
        changed = true;
    }
    if let Some(run) = &s.load_run {
        let snap = run.snapshot();
        let done = snap.done;
        if s.load.as_ref() != Some(&snap) {
            s.load = Some(snap);
            changed = true;
        }
        if done {
            s.load_run = None;
        }
    }
    s.busy.store(s.load_running(), Ordering::Relaxed);
    changed
}

fn main() {
    rune::run(AppSpec::<State> {
        scenes: &[
            ("demo", demo),
            ("empty", |s| {
                s.offline = true;
                s.actions.clear();
            }),
        ],
        setup: Some(setup),
        poll: Some(poll),
        min_size: Some((640.0, 420.0)),
        ..AppSpec::new("rust-build", (760.0, 520.0), init, view::view)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};

    fn runtime(scene: fn(&mut State)) -> Runtime<State> {
        let mut s = init();
        scene(&mut s);
        let mut rt = Runtime::new(s, view::view, (760.0, 520.0), Env::new(true, 2.0));
        rt.poll = Some(poll);
        rt.frame(0.0);
        rt
    }

    fn press(rt: &mut Runtime<State>, key: &str) {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("{key} is on screen"));
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
    }

    #[test]
    fn demo_scene_shows_health_load_and_every_traffic_row() {
        let rt = runtime(demo);
        for key in ["health-card", "ws-card", "load-card", "traffic-header", "traffic-0", "traffic-1", "traffic-2"] {
            assert!(rt.find(key).is_some(), "{key} rendered");
        }
        assert!(rt.find("traffic-3").is_none());
    }

    #[test]
    fn buttons_queue_actions_for_poll() {
        let mut rt = runtime(demo);
        // Offline poll drains actions without side effects, so record them first.
        rt.poll = None;
        press(&mut rt, "load-button");
        press(&mut rt, "ping-button");
        press(&mut rt, "embedded-button");
        press(&mut rt, "refresh-button");
        assert_eq!(rt.state.actions, vec![Action::StartLoad, Action::PingWs, Action::ToggleEmbedded, Action::Refresh]);
    }

    #[test]
    fn empty_scene_renders_placeholders() {
        let rt = runtime(|s| {
            s.offline = true;
            s.actions.clear();
        });
        assert!(rt.find("traffic-empty").is_some());
        assert!(rt.find("traffic-0").is_none());
    }

    #[test]
    fn backend_talks_to_an_embedded_server() {
        let server = backend::start_embedded().expect("embedded server");
        let fetched = backend::fetch(&server.url);
        assert!(matches!(&fetched.health, Health::Up { http_version, .. } if http_version == "HTTP/1.1"), "{:?}", fetched.health);
        let fetched = backend::fetch(&server.url);
        assert!(fetched.traffic.is_some_and(|t| t.iter().any(|r| r.path == "/api/info" && r.count >= 1)));
        assert!(backend::ws_ping(&server.url).is_ok());
        let run = backend::start_load(&server.url).expect("load");
        run.stop();
        let snap = backend::rt().block_on(run.wait());
        assert!(snap.done && snap.errors == 0, "{snap:?}");
    }

    #[test]
    fn traffic_average_latency() {
        let row = TrafficRow { count: 4, total_nanos: 8_000_000, ..Default::default() };
        assert!((row.avg_ms() - 2.0).abs() < 1e-9);
        assert_eq!(TrafficRow::default().avg_ms(), 0.0);
    }
}
