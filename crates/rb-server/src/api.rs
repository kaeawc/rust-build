use crate::error::ApiError;
use crate::state::AppState;
use crate::store::{NewUser, User};
use axum::Json;
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, MatchedPath, Query, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use rb_core::paginator::{self, Page};
use rb_tokens::StandardClaims;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::request_id::{MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

const USERS_GEN: &str = "users:gen";
const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Builds the full application router.
///
/// - `GET /healthz` liveness (cached), `GET /readyz` readiness (fails while shutting down)
/// - `GET /api/info` protocol/peer details (handy for checking HTTP/2 negotiation)
/// - `GET|POST /api/users` cursor-paginated list (cached) / create
/// - `POST /api/token`, `GET /api/me` HS256 bearer tokens
/// - `GET /api/admin/traffic`, `GET /api/admin/healthchecks`
/// - `/ws` WebSocket echo (HTTP/1.1 upgrade or HTTP/2 extended CONNECT)
/// - `GET /metrics` Prometheus/OpenMetrics exposition
///
/// Anything else is a JSON 404.
pub fn router(state: AppState) -> Router {
    build(state, None)
}

/// [`router`], with `fallback` answering every path no route matches, except under `/api`,
/// which keeps its JSON 404. The fallback runs inside the same layers (request ids, tracing,
/// timeouts, compression, security headers). This is how `rb serve --features web` mounts the
/// embedded web UI.
pub fn router_with_fallback(state: AppState, fallback: Router) -> Router {
    build(state, Some(fallback))
}

fn build(state: AppState, fallback: Option<Router>) -> Router {
    let api = Router::new()
        .route("/info", get(info))
        .route("/users", get(list_users).post(create_user))
        .route("/token", post(issue_token))
        .route("/me", get(me))
        .route("/admin/traffic", get(admin_traffic))
        .route("/admin/healthchecks", get(admin_healthchecks))
        .route_layer(middleware::from_fn_with_state(state.clone(), track_traffic))
        // Unknown `/api/*` paths stay JSON 404s even when a fallback serves everything else.
        .fallback(|| async { ApiError::not_found() });

    let ids = Ids(state.clone());
    let timeout = state.0.cfg.request_timeout;
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/ws", any(ws))
        .route("/metrics", get(metrics))
        .nest("/api", api);
    let app = match fallback {
        Some(f) => app.fallback_service(f),
        None => app.fallback(|| async { ApiError::not_found() }),
    };
    app.layer(
        ServiceBuilder::new()
            .layer(SetRequestIdLayer::new(REQUEST_ID, ids))
            .layer(TraceLayer::new_for_http().make_span_with(make_span))
            .layer(PropagateRequestIdLayer::new(REQUEST_ID))
            .layer(CatchPanicLayer::custom(|_| ApiError::internal("handler panicked").into_response()))
            .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, timeout))
            .layer(CompressionLayer::new().gzip(true))
            .layer(SetResponseHeaderLayer::if_not_present(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
            .layer(SetResponseHeaderLayer::if_not_present(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")))
            .layer(SetResponseHeaderLayer::if_not_present(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"))),
    )
    .with_state(state)
}

#[derive(Clone)]
struct Ids(AppState);

impl MakeRequestId for Ids {
    fn make_request_id<B>(&mut self, _req: &http::Request<B>) -> Option<RequestId> {
        HeaderValue::from_str(&self.0.0.ids.next_id()).ok().map(RequestId::new)
    }
}

/// One span per request, carrying the request id and continuing the caller's trace (`traceparent`)
/// when built with `otel`.
fn make_span(req: &http::Request<axum::body::Body>) -> tracing::Span {
    let request_id = req.headers().get(&REQUEST_ID).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let span = tracing::info_span!("request", method = %req.method(), uri = %req.uri(), version = ?req.version(), request_id);
    rb_telemetry::propagation::extract(&span, req.headers());
    span
}

async fn track_traffic(State(s): State<AppState>, matched: Option<MatchedPath>, req: Request, next: Next) -> Response {
    let method = req.method().to_string();
    let path = matched.map_or_else(|| "(unmatched)".to_string(), |m| m.as_str().to_string());
    let start = s.0.clock.now();
    s.0.metrics.http_in_flight.inc();
    let resp = next.run(req).await;
    s.0.metrics.http_in_flight.dec();
    let elapsed = s.0.clock.now().saturating_duration_since(start);
    let status = resp.status().as_u16();
    s.0.traffic.record(&method, &path, status, elapsed);
    s.0.metrics.observe_http(&method, &path, status, elapsed);
    resp
}

async fn metrics(State(s): State<AppState>) -> Response {
    ([(header::CONTENT_TYPE, rb_telemetry::metrics::CONTENT_TYPE)], s.metrics().render()).into_response()
}

/// Body of `/healthz` and `/readyz` (`probes` only on `/readyz`).
#[derive(Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, optional_fields))]
pub struct HealthStatus {
    /// `ok`, `unhealthy` or `shutting_down`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probes: Option<Vec<crate::ProbeResult>>,
}

async fn healthz(State(s): State<AppState>) -> Response {
    let no_store = [(header::CACHE_CONTROL, "no-store")];
    if s.healthy_cached().await {
        (no_store, Json(HealthStatus { status: "ok", probes: None })).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, no_store, Json(HealthStatus { status: "unhealthy", probes: None })).into_response()
    }
}

async fn readyz(State(s): State<AppState>) -> Response {
    if s.is_shutting_down() {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(HealthStatus { status: "shutting_down", probes: None })).into_response();
    }
    let probes = s.probe_all().await;
    let ok = probes.iter().all(|p| p.healthy);
    let status = if ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (status, Json(HealthStatus { status: if ok { "ok" } else { "unhealthy" }, probes: Some(probes) })).into_response()
}

/// Body of `/api/info`.
#[derive(Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub http_version: String,
    pub peer: Option<String>,
    pub uptime_secs: u64,
    pub version: &'static str,
}

async fn info(State(s): State<AppState>, req: Request) -> Json<Info> {
    let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|ConnectInfo(a)| a.to_string());
    Json(Info {
        http_version: format!("{:?}", req.version()),
        peer,
        uptime_secs: s.0.clock.now().saturating_duration_since(s.0.started).as_secs(),
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    page_size: Option<String>,
    cursor: Option<String>,
}

async fn list_users(State(s): State<AppState>, Query(q): Query<ListQuery>) -> Result<Response, ApiError> {
    let size = paginator::clamp_page_size(q.page_size.as_deref(), 20, 100);
    let after = match q.cursor.as_deref() {
        Some(c) => Some(paginator::decode::<i64>(c).map_err(|e| ApiError::bad_request(e.to_string()))?),
        None => None,
    };
    // Generation-keyed: a write bumps `users:gen`, so older pages are simply never read again.
    let generation = s.0.cache.get(USERS_GEN).await.ok().flatten().unwrap_or_default();
    let key = format!("users:{generation}:{size}:{after:?}");
    if let Ok(Some(body)) = s.0.cache.get(&key).await {
        return Ok(([(header::CONTENT_TYPE, "application/json")], [(HeaderName::from_static("x-cache"), "hit")], body).into_response());
    }
    let rows = s.0.store.list(after, size + 1).await.map_err(|e| ApiError::internal(e.0))?;
    let page = Page::from_fetch(rows, size, |u: &User| paginator::encode(&u.id));
    let body = serde_json::to_string(&page).map_err(|e| ApiError::internal(e.to_string()))?;
    if let Err(e) = s.0.cache.set(&key, body.clone(), Some(s.0.cfg.users_cache_ttl)).await {
        tracing::warn!(error = %e, "cache write failed");
    }
    Ok(([(header::CONTENT_TYPE, "application/json")], [(HeaderName::from_static("x-cache"), "miss")], body).into_response())
}

async fn create_user(State(s): State<AppState>, Json(new): Json<NewUser>) -> Result<(StatusCode, Json<User>), ApiError> {
    let name = new.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(ApiError::bad_request("name must be 1-100 characters"));
    }
    let user = s.0.store.create(NewUser { name: name.to_string() }).await.map_err(|e| ApiError::internal(e.0))?;
    if let Some(jobs) = &s.0.jobs
        && let Err(e) = jobs.insert(&crate::UserCreated { user_id: user.id, name: user.name.clone() }).await
    {
        tracing::warn!(error = %e, user_id = user.id, "enqueueing user_created failed");
    }
    // Any write invalidates every cached page.
    if let Err(e) = s.0.cache.incr(USERS_GEN).await {
        tracing::warn!(error = %e, "cache invalidation failed");
    }
    Ok((StatusCode::CREATED, Json(user)))
}

/// Body of `POST /api/token`.
#[derive(Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, optional_fields))]
#[serde(rename_all = "camelCase")]
pub struct TokenRequest {
    pub sub: String,
    pub ttl_secs: Option<u64>,
}

/// Response of `POST /api/token`.
#[derive(Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    pub token: String,
    pub expires_at: u64,
}

async fn issue_token(State(s): State<AppState>, Json(req): Json<TokenRequest>) -> Result<Json<TokenResponse>, ApiError> {
    if req.sub.is_empty() {
        return Err(ApiError::bad_request("sub is required"));
    }
    let now = s.tokens().now_unix();
    let exp = now + req.ttl_secs.unwrap_or(3600).min(86_400);
    let claims =
        StandardClaims { sub: Some(req.sub), iat: Some(now), exp: Some(exp), iss: Some("rust-build".into()), ..Default::default() };
    let token = s.tokens().sign(&claims).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(TokenResponse { token, expires_at: exp }))
}

async fn me(State(s): State<AppState>, headers: HeaderMap) -> Result<Json<StandardClaims>, ApiError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
    s.tokens().verify::<StandardClaims>(token).map(Json).map_err(|e| ApiError::unauthorized(e.to_string()))
}

async fn admin_traffic(State(s): State<AppState>) -> impl IntoResponse {
    Json(s.traffic().snapshot())
}

async fn admin_healthchecks(State(s): State<AppState>) -> impl IntoResponse {
    Json(s.probe_all().await)
}

async fn ws(State(s): State<AppState>, up: WebSocketUpgrade) -> Response {
    let metrics = s.metrics().clone();
    up.on_upgrade(move |socket| async move {
        metrics.ws_connections.inc();
        echo(socket, &metrics).await;
        metrics.ws_connections.dec();
    })
}

async fn echo(mut socket: WebSocket, metrics: &rb_telemetry::metrics::Metrics) {
    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            Message::Text(_) | Message::Binary(_) => {
                metrics.ws_messages.inc();
                if socket.send(msg).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }
}
