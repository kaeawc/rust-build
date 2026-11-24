//! Prometheus/OpenMetrics metrics.
//!
//! One [`Metrics`] per process, shared by `Arc`. Label sets are bounded by design: HTTP routes are
//! the *matched* route templates, never raw paths.

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
use prometheus_client::metrics::info::Info;
use prometheus_client::registry::Registry;
use std::sync::Mutex;
use std::time::Duration;

/// `Content-Type` for [`Metrics::render`].
pub const CONTENT_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct HttpLabels {
    pub method: String,
    pub route: String,
    pub status: u16,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RouteLabels {
    pub method: String,
    pub route: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TunnelLabels {
    /// `server` or `client`.
    pub side: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct DirectionLabels {
    pub side: String,
    /// `up` (accepting side → far side) or `down`.
    pub direction: String,
}

fn latency_histogram() -> Histogram {
    // 0.25 ms … ~8 s.
    Histogram::new(exponential_buckets(0.000_25, 2.0, 16))
}

pub struct Metrics {
    registry: Mutex<Registry>,
    pub http_requests: Family<HttpLabels, Counter>,
    pub http_duration: Family<RouteLabels, Histogram>,
    pub http_in_flight: Gauge,
    pub ws_connections: Gauge,
    pub ws_messages: Counter,
    pub tunnel_connections: Family<TunnelLabels, Counter>,
    pub tunnel_active: Family<TunnelLabels, Gauge>,
    pub tunnel_failures: Family<TunnelLabels, Counter>,
    pub tunnel_bytes: Family<DirectionLabels, Counter>,
    pub tunnel_streams: Family<TunnelLabels, Counter>,
    pub tunnel_denied: Family<TunnelLabels, Counter>,
    pub tunnel_idle_closed: Family<TunnelLabels, Counter>,
}

impl Metrics {
    /// A registry under `prefix` (e.g. `rb`) with build info for `version`.
    pub fn new(prefix: &str, version: &str) -> Self {
        let mut root = Registry::with_prefix(prefix);
        root.register("build", "Build information", Info::new(vec![("version".to_string(), version.to_string())]));

        let http_requests = Family::<HttpLabels, Counter>::default();
        let http_duration = Family::<RouteLabels, Histogram>::new_with_constructor(latency_histogram as fn() -> Histogram);
        let http_in_flight = Gauge::default();
        root.register("http_requests", "HTTP requests by matched route and status", http_requests.clone());
        root.register("http_request_duration_seconds", "HTTP request latency by matched route", http_duration.clone());
        root.register("http_requests_in_flight", "HTTP requests being served", http_in_flight.clone());

        let ws_connections = Gauge::default();
        let ws_messages = Counter::default();
        root.register("websocket_connections", "Open WebSocket connections", ws_connections.clone());
        root.register("websocket_messages", "WebSocket messages echoed", ws_messages.clone());

        let tunnel_connections = Family::<TunnelLabels, Counter>::default();
        let tunnel_active = Family::<TunnelLabels, Gauge>::default();
        let tunnel_failures = Family::<TunnelLabels, Counter>::default();
        let tunnel_bytes = Family::<DirectionLabels, Counter>::default();
        root.register("tunnel_connections", "Tunnel connections accepted", tunnel_connections.clone());
        root.register("tunnel_connections_active", "Tunnel connections open", tunnel_active.clone());
        root.register("tunnel_failures", "Tunnel connections that failed", tunnel_failures.clone());
        root.register("tunnel_bytes", "Bytes relayed through tunnels", tunnel_bytes.clone());
        let tunnel_streams = Family::<TunnelLabels, Counter>::default();
        let tunnel_denied = Family::<TunnelLabels, Counter>::default();
        let tunnel_idle_closed = Family::<TunnelLabels, Counter>::default();
        root.register("tunnel_streams", "Multiplexed tunnel streams opened (HTTP/2 mode)", tunnel_streams.clone());
        root.register("tunnel_denied", "Tunnel streams refused by the target allowlist", tunnel_denied.clone());
        root.register("tunnel_idle_closed", "Tunnel streams closed by the idle timeout", tunnel_idle_closed.clone());

        Metrics {
            registry: Mutex::new(root),
            http_requests,
            http_duration,
            http_in_flight,
            ws_connections,
            ws_messages,
            tunnel_connections,
            tunnel_active,
            tunnel_failures,
            tunnel_bytes,
            tunnel_streams,
            tunnel_denied,
            tunnel_idle_closed,
        }
    }

    /// Records one finished HTTP request.
    pub fn observe_http(&self, method: &str, route: &str, status: u16, elapsed: Duration) {
        self.http_requests.get_or_create(&HttpLabels { method: method.into(), route: route.into(), status }).inc();
        self.http_duration.get_or_create(&RouteLabels { method: method.into(), route: route.into() }).observe(elapsed.as_secs_f64());
    }

    /// Registers extra application metrics.
    pub fn with_registry<R>(&self, f: impl FnOnce(&mut Registry) -> R) -> R {
        f(&mut self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    /// OpenMetrics text exposition.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let registry = self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Writing into a String never fails.
        let _ = encode(&mut out, &registry);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_http_and_build_info() {
        let m = Metrics::new("rb", "1.2.3");
        m.observe_http("GET", "/api/users", 200, Duration::from_millis(3));
        m.observe_http("GET", "/api/users", 200, Duration::from_millis(5));
        m.http_in_flight.inc();
        m.tunnel_bytes.get_or_create(&DirectionLabels { side: "client".into(), direction: "up".into() }).inc_by(17);
        let text = m.render();
        assert!(text.contains(r#"rb_build_info{version="1.2.3"} 1"#), "{text}");
        assert!(text.contains(r#"rb_http_requests_total{method="GET",route="/api/users",status="200"} 2"#), "{text}");
        assert!(text.contains(r#"rb_http_request_duration_seconds_count{method="GET",route="/api/users"} 2"#), "{text}");
        assert!(text.contains("rb_http_requests_in_flight 1"), "{text}");
        assert!(text.contains(r#"rb_tunnel_bytes_total{side="client",direction="up"} 17"#), "{text}");
        assert!(text.trim_end().ends_with("# EOF"));
    }
}
