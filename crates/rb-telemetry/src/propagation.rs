//! W3C trace-context propagation. With the `otel` feature, [`extract`] makes a server span a child
//! of the caller's `traceparent` and [`inject`] adds the current span's context to outgoing
//! requests. Without it both are no-ops, so call sites don't need `cfg`.

use http::HeaderMap;

/// Parents `span` on the trace context carried by `headers`, if any.
#[cfg(feature = "otel")]
pub fn extract(span: &tracing::Span, headers: &HeaderMap) {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    let cx = opentelemetry::global::get_text_map_propagator(|p| p.extract(&opentelemetry_http::HeaderExtractor(headers)));
    let _ = span.set_parent(cx);
}

/// Adds the current span's trace context (`traceparent`, `tracestate`) to `headers`.
#[cfg(feature = "otel")]
pub fn inject(headers: &mut HeaderMap) {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    let cx = tracing::Span::current().context();
    opentelemetry::global::get_text_map_propagator(|p| p.inject_context(&cx, &mut opentelemetry_http::HeaderInjector(headers)));
}

#[cfg(not(feature = "otel"))]
pub fn extract(_span: &tracing::Span, _headers: &HeaderMap) {}

#[cfg(not(feature = "otel"))]
pub fn inject(_headers: &mut HeaderMap) {}
