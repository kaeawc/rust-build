//! Telemetry for every rust-build process.
//!
//! - [`init`]: `tracing` subscriber with text or JSON logs, plus (feature `otel`) an OTLP/HTTP
//!   span exporter when an endpoint is configured.
//! - [`metrics::Metrics`]: Prometheus/OpenMetrics registry with the HTTP, WebSocket and tunnel
//!   families the server and tunnel record into, rendered by `/metrics`.
//! - [`propagation`]: W3C `traceparent` extract/inject (no-ops without `otel`), so a trace
//!   continues across `rb` client → server hops.

pub mod metrics;
pub mod propagation;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable, colored on a TTY.
    #[default]
    Text,
    /// One JSON object per line (for log shippers).
    Json,
}

impl std::str::FromStr for LogFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "text" | "pretty" => Ok(LogFormat::Text),
            "json" => Ok(LogFormat::Json),
            other => Err(format!("unknown log format {other:?} (text|json)")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    /// `EnvFilter` directives, e.g. `info,rb_server=debug`.
    pub filter: String,
    pub format: LogFormat,
    /// OTLP/HTTP base endpoint (e.g. `http://localhost:4318`). Ignored without the `otel` feature.
    pub otlp_endpoint: Option<String>,
    pub service_name: String,
}

impl Default for Config {
    fn default() -> Self {
        Config { filter: "info".into(), format: LogFormat::Text, otlp_endpoint: None, service_name: "rust-build".into() }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("installing the tracing subscriber: {0}")]
    Subscriber(String),
    #[error("building the OTLP exporter: {0}")]
    Otlp(String),
}

/// Flushes and shuts down exporters when dropped. Keep it alive for the life of `main`.
#[must_use = "dropping the guard shuts telemetry down"]
pub struct Guard {
    #[cfg(feature = "otel")]
    provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(feature = "otel")]
        if let Some(p) = self.provider.take() {
            let _ = p.shutdown();
        }
    }
}

/// Whether this build can export traces.
pub const OTEL_ENABLED: bool = cfg!(feature = "otel");

/// Installs the global subscriber. Call once, early in `main`. With an OTLP endpoint it must run
/// inside a tokio runtime: spans are exported on it.
pub fn init(cfg: &Config) -> Result<Guard, Error> {
    let filter = EnvFilter::try_new(&cfg.filter).unwrap_or_else(|_| EnvFilter::new("info"));
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let fmt = match cfg.format {
        LogFormat::Text => tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_ansi(ansi).compact().boxed(),
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(false)
            .boxed(),
    };
    // The filter applies to logs only; trace export has its own (info) so quieting logs with
    // `--log warn` doesn't also drop spans.
    let registry = tracing_subscriber::registry().with(fmt.with_filter(filter));

    #[cfg(feature = "otel")]
    {
        let (layer, provider) = match &cfg.otlp_endpoint {
            Some(endpoint) => {
                let (layer, provider) = otel::layer(endpoint, &cfg.service_name)?;
                (Some(layer.with_filter(EnvFilter::new("info"))), Some(provider))
            }
            None => (None, None),
        };
        registry.with(layer).try_init().map_err(|e| Error::Subscriber(e.to_string()))?;
        Ok(Guard { provider })
    }

    #[cfg(not(feature = "otel"))]
    {
        registry.try_init().map_err(|e| Error::Subscriber(e.to_string()))?;
        if cfg.otlp_endpoint.is_some() {
            tracing::warn!("an OTLP endpoint is set but this build lacks the `otel` feature; traces are not exported");
        }
        Ok(Guard {})
    }
}

use tracing_subscriber::Layer as _;

#[cfg(feature = "otel")]
mod otel {
    use super::Error;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::{WithExportConfig as _, WithHttpConfig as _};
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::trace::SdkTracerProvider;

    pub type Layer<S> = tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::SdkTracer>;

    pub fn layer<S>(endpoint: &str, service: &str) -> Result<(Layer<S>, SdkTracerProvider), Error>
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        let url = format!("{}/v1/traces", endpoint.trim_end_matches('/'));
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_http_client(opentelemetry_http::hyper::HyperClient::with_default_connector(std::time::Duration::from_secs(10), None))
            .with_endpoint(url)
            .build()
            .map_err(|e| Error::Otlp(e.to_string()))?;
        // Export on the tokio runtime (the hyper client needs it), not on a dedicated thread.
        let batch = opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor::builder(
            exporter,
            opentelemetry_sdk::runtime::Tokio,
        )
        .build();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(batch)
            .with_resource(Resource::builder().with_service_name(service.to_string()).build())
            .build();
        opentelemetry::global::set_text_map_propagator(opentelemetry_sdk::propagation::TraceContextPropagator::new());
        opentelemetry::global::set_tracer_provider(provider.clone());
        let tracer = provider.tracer(service.to_string());
        Ok((tracing_opentelemetry::layer().with_tracer(tracer), provider))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_log_formats() {
        assert_eq!("json".parse::<LogFormat>(), Ok(LogFormat::Json));
        assert_eq!("TEXT".parse::<LogFormat>(), Ok(LogFormat::Text));
        assert!("xml".parse::<LogFormat>().is_err());
    }
}
