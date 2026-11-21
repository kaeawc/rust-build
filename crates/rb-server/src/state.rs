use crate::health::{Probe, ProbeFuture, ProbeResult, run_all};
use crate::store::{MemoryStore, UserStore};
use crate::traffic::TrafficRecorder;
use rb_core::clock::{SharedClock, System};
use rb_core::idgen::{IdGen, Random};
use rb_core::kv::Memory;
use rb_telemetry::metrics::Metrics;
use rb_tokens::Hmac256;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub struct Config {
    pub store: Arc<dyn UserStore>,
    pub clock: SharedClock,
    pub ids: Arc<dyn IdGen>,
    pub token_secret: Vec<u8>,
    /// How long `/api/users` responses stay cached.
    pub users_cache_ttl: Duration,
    /// How long `/healthz`'s result is reused (keeps aggressive probers from hammering the store).
    pub health_cache_ttl: Duration,
    pub probe_timeout: Duration,
    pub request_timeout: Duration,
    /// Extra readiness probes on top of the built-in store and cache probes.
    pub probes: Vec<Arc<dyn Probe>>,
    /// Prometheus registry served at `/metrics`; share it to add process-wide metrics.
    pub metrics: Arc<Metrics>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            store: Arc::new(MemoryStore::seeded(&["Ada", "Grace", "Linus", "Barbara", "Ken"])),
            clock: Arc::new(System),
            ids: Arc::new(Random::default()),
            token_secret: b"dev-secret-change-me".to_vec(),
            users_cache_ttl: Duration::from_secs(30),
            health_cache_ttl: Duration::from_secs(5),
            probe_timeout: Duration::from_secs(2),
            request_timeout: Duration::from_secs(10),
            probes: Vec::new(),
            metrics: Arc::new(Metrics::new("rb", env!("CARGO_PKG_VERSION"))),
        }
    }
}

/// Shared, cheaply clonable application state.
#[derive(Clone)]
pub struct AppState(pub(crate) Arc<Inner>);

pub(crate) struct Inner {
    pub store: Arc<dyn UserStore>,
    pub cache: Arc<Memory<String>>,
    pub traffic: TrafficRecorder,
    pub tokens: Hmac256,
    pub clock: SharedClock,
    pub ids: Arc<dyn IdGen>,
    pub probes: Vec<Arc<dyn Probe>>,
    pub metrics: Arc<Metrics>,
    pub started: Instant,
    pub shutting_down: AtomicBool,
    pub health: std::sync::Mutex<Option<(Instant, bool)>>,
    pub cfg: Timeouts,
}

pub(crate) struct Timeouts {
    pub users_cache_ttl: Duration,
    pub health_cache_ttl: Duration,
    pub probe_timeout: Duration,
    pub request_timeout: Duration,
}

struct StoreProbe(Arc<dyn UserStore>);

impl Probe for StoreProbe {
    fn name(&self) -> &str {
        "store"
    }
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async move { self.0.ping().await.map_err(|e| e.0) })
    }
}

struct CacheProbe(Arc<Memory<String>>);

impl Probe for CacheProbe {
    fn name(&self) -> &str {
        "cache"
    }
    fn check(&self) -> ProbeFuture<'_> {
        self.0.set("_probe", "ok".into(), Some(Duration::from_secs(1)));
        let r = if self.0.get("_probe").is_some() { Ok(()) } else { Err("cache read-back failed".into()) };
        Box::pin(std::future::ready(r))
    }
}

impl AppState {
    pub fn new(cfg: Config) -> Self {
        let cache = Arc::new(Memory::new(cfg.clock.clone()));
        let mut probes: Vec<Arc<dyn Probe>> = vec![Arc::new(StoreProbe(cfg.store.clone())), Arc::new(CacheProbe(cache.clone()))];
        probes.extend(cfg.probes);
        AppState(Arc::new(Inner {
            store: cfg.store,
            cache,
            traffic: TrafficRecorder::default(),
            tokens: Hmac256::new(&cfg.token_secret).with_clock(cfg.clock.clone()),
            started: cfg.clock.now(),
            clock: cfg.clock,
            ids: cfg.ids,
            probes,
            metrics: cfg.metrics,
            shutting_down: AtomicBool::new(false),
            health: std::sync::Mutex::new(None),
            cfg: Timeouts {
                users_cache_ttl: cfg.users_cache_ttl,
                health_cache_ttl: cfg.health_cache_ttl,
                probe_timeout: cfg.probe_timeout,
                request_timeout: cfg.request_timeout,
            },
        }))
    }

    /// Flips readiness to failing so load balancers drain before the listener closes.
    pub fn set_shutting_down(&self) {
        self.0.shutting_down.store(true, Ordering::SeqCst);
    }

    pub fn is_shutting_down(&self) -> bool {
        self.0.shutting_down.load(Ordering::SeqCst)
    }

    pub fn traffic(&self) -> &TrafficRecorder {
        &self.0.traffic
    }

    pub fn metrics(&self) -> &Arc<Metrics> {
        &self.0.metrics
    }

    pub fn tokens(&self) -> &Hmac256 {
        &self.0.tokens
    }

    pub async fn probe_all(&self) -> Vec<ProbeResult> {
        run_all(&self.0.probes, self.0.cfg.probe_timeout).await
    }

    /// Liveness from the store probe, cached for `health_cache_ttl`.
    pub(crate) async fn healthy_cached(&self) -> bool {
        let now = self.0.clock.now();
        if let Some((at, ok)) = *rb_core::clock::lock(&self.0.health)
            && now.duration_since(at) < self.0.cfg.health_cache_ttl
        {
            return ok;
        }
        let ok = tokio::time::timeout(Duration::from_secs(1), self.0.store.ping()).await.is_ok_and(|r| r.is_ok());
        *rb_core::clock::lock(&self.0.health) = Some((now, ok));
        ok
    }
}
