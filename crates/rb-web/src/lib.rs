//! The web UI (`web/dist`), embedded in the binary and served as a single-page app.
//!
//! | Request | Response |
//! | --- | --- |
//! | `/assets/<hashed file>` | 200, `Cache-Control: public, max-age=31536000, immutable` |
//! | `/assets/<missing>`, or a missing path with a file extension | 404 (never `index.html`: an old tab asking for a deleted chunk must fail loudly, not get HTML) |
//! | another existing file (`favicon.ico`, `robots.txt`) | 200, `Cache-Control: public, max-age=3600` |
//! | anything else (a client-side route) | `index.html`, `Cache-Control: no-cache`, so a redeploy shows up on the next navigation |
//! | `If-None-Match` matches | 304 |
//! | `Accept-Encoding: br` / `gzip` and a `.br` / `.gz` sibling exists | that variant, with `Content-Encoding` and `Vary: Accept-Encoding` |
//! | UI not built when the binary was | 503 `web UI not built` |
//!
//! ETags are SHA-256 (ring) of each representation. HTML gets [`CSP`]. `/api/*` never reaches
//! this: `rb_server::router_with_fallback` keeps JSON 404s there.

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use include_dir::{Dir, include_dir};
use std::collections::HashMap;
use std::sync::Arc;

static DIST: Dir<'static> = include_dir!("$RB_WEB_DIST");

/// Content-Security-Policy for HTML. Vite emits no inline scripts. Radix's scroll lock
/// (react-remove-scroll) injects a `<style>` element when a dialog opens, which needs
/// `'unsafe-inline'` in `style-src` (verified by the Playwright suite in `web/e2e`).
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                       font-src 'self'; connect-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'self'; \
                       form-action 'self'";

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const SHORT: &str = "public, max-age=3600";
const NO_CACHE: &str = "no-cache";

struct Variant {
    body: &'static [u8],
    etag: HeaderValue,
}

struct Asset {
    identity: Variant,
    br: Option<Variant>,
    gzip: Option<Variant>,
    content_type: &'static str,
}

/// The asset table, built once at startup.
pub struct Spa {
    assets: HashMap<String, Asset>,
}

fn etag(body: &[u8], suffix: &str) -> HeaderValue {
    let digest = ring::digest::digest(&ring::digest::SHA256, body);
    let hex: String = digest.as_ref()[..16].iter().map(|b| format!("{b:02x}")).collect();
    HeaderValue::from_str(&format!("\"{hex}{suffix}\"")).unwrap_or(HeaderValue::from_static("\"0\""))
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn walk(dir: &'static Dir<'static>, out: &mut Vec<(String, &'static [u8])>) {
    for f in dir.files() {
        out.push((f.path().to_string_lossy().replace('\\', "/"), f.contents()));
    }
    for d in dir.dirs() {
        walk(d, out);
    }
}

/// Which encodings the client accepts (a `q=0` entry is a refusal).
fn accepts(headers: &HeaderMap, coding: &str) -> bool {
    headers.get_all(header::ACCEPT_ENCODING).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(',')).any(|item| {
        let mut parts = item.split(';').map(str::trim);
        let name = parts.next().unwrap_or_default();
        let refused = parts.any(|p| p.strip_prefix("q=").is_some_and(|q| q.parse::<f32>().is_ok_and(|q| q <= 0.0)));
        name.eq_ignore_ascii_case(coding) && !refused
    })
}

fn not_modified(headers: &HeaderMap, etag: &HeaderValue) -> bool {
    let Some(inm) = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let ours = etag.to_str().unwrap_or_default();
    inm.split(',').map(str::trim).any(|t| t == "*" || t.trim_start_matches("W/") == ours)
}

fn text(status: StatusCode, body: &'static str) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CACHE_CONTROL, NO_CACHE)], body).into_response()
}

impl Spa {
    /// The UI embedded at build time from `web/dist` (empty if it wasn't built).
    pub fn embedded() -> Self {
        let mut files = Vec::new();
        walk(&DIST, &mut files);
        Self::from_files(files)
    }

    /// An asset table from `(relative path, contents)` pairs; `x.br` / `x.gz` become variants of `x`.
    pub fn from_files(files: impl IntoIterator<Item = (String, &'static [u8])>) -> Self {
        let files: HashMap<String, &'static [u8]> = files.into_iter().collect();
        let mut assets = HashMap::new();
        for (path, body) in &files {
            if path.ends_with(".br") || path.ends_with(".gz") {
                continue;
            }
            let variant =
                |ext: &str, suffix: &str| files.get(&format!("{path}.{ext}")).map(|b| Variant { body: b, etag: etag(body, suffix) });
            let asset = Asset {
                identity: Variant { body, etag: etag(body, "") },
                br: variant("br", ".br"),
                gzip: variant("gz", ".gz"),
                content_type: content_type(path),
            };
            assets.insert(path.clone(), asset);
        }
        Spa { assets }
    }

    /// Whether `index.html` is present, i.e. the UI was built.
    pub fn is_built(&self) -> bool {
        self.assets.contains_key("index.html")
    }

    /// Answers one request by the rules in the crate docs.
    pub fn respond(&self, method: &Method, path: &str, headers: &HeaderMap) -> Response {
        if method != Method::GET && method != Method::HEAD {
            return (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "GET, HEAD")]).into_response();
        }
        if !self.is_built() {
            return text(StatusCode::SERVICE_UNAVAILABLE, "web UI not built (run `make web`, then rebuild rb with --features web)\n");
        }
        let rel = path.trim_start_matches('/');
        let last = rel.rsplit('/').next().unwrap_or_default();
        let (asset, cache) = match self.assets.get(rel) {
            Some(a) if rel == "index.html" => (a, NO_CACHE),
            Some(a) if rel.starts_with("assets/") => (a, IMMUTABLE),
            Some(a) => (a, SHORT),
            None if rel.starts_with("assets/") || last.contains('.') => return text(StatusCode::NOT_FOUND, "not found\n"),
            None => match self.assets.get("index.html") {
                Some(index) => (index, NO_CACHE),
                None => return text(StatusCode::NOT_FOUND, "not found\n"),
            },
        };
        let (variant, encoding) = match (&asset.br, &asset.gzip) {
            (Some(br), _) if accepts(headers, "br") => (br, Some("br")),
            (_, Some(gz)) if accepts(headers, "gzip") => (gz, Some("gzip")),
            _ => (&asset.identity, None),
        };
        let mut h = HeaderMap::new();
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
        h.insert(header::ETAG, variant.etag.clone());
        if asset.br.is_some() || asset.gzip.is_some() {
            h.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
        }
        if not_modified(headers, &variant.etag) {
            return (StatusCode::NOT_MODIFIED, h).into_response();
        }
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static(asset.content_type));
        if let Some(enc) = encoding {
            h.insert(header::CONTENT_ENCODING, HeaderValue::from_static(enc));
        }
        if asset.content_type.starts_with("text/html") {
            h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
        }
        let body = if method == Method::HEAD { Body::empty() } else { Body::from(variant.body) };
        let mut resp = (StatusCode::OK, h, body).into_response();
        if method == Method::HEAD {
            resp.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(variant.body.len()));
        }
        resp
    }

    /// A router that answers every request from this table; mount it as a fallback.
    pub fn router(self) -> Router {
        let spa = Arc::new(self);
        Router::new().fallback(move |req: Request| {
            let spa = spa.clone();
            async move { spa.respond(req.method(), req.uri().path(), req.headers()) }
        })
    }
}

/// The embedded UI as a router (see [`Spa::embedded`]).
pub fn router() -> Router {
    let spa = Spa::embedded();
    if !spa.is_built() {
        tracing::warn!("web/dist was not built when this binary was; the web UI answers 503 (run `make web` and rebuild)");
    }
    spa.router()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn fixture() -> Spa {
        Spa::from_files([
            ("index.html".to_string(), &b"<!doctype html><title>rb</title>"[..]),
            ("assets/app-abc123.js".into(), b"console.log('app')"),
            ("assets/app-abc123.js.br".into(), b"BR"),
            ("assets/app-abc123.js.gz".into(), b"GZ"),
            ("assets/app-abc123.css".into(), b"body{}"),
            ("favicon.ico".into(), b"ICO"),
        ])
    }

    async fn get(spa: &Spa, method: Method, path: &str, headers: &[(&'static str, &str)]) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut h = HeaderMap::new();
        for (k, v) in headers {
            h.append(*k, HeaderValue::from_str(v).expect("header"));
        }
        let resp = spa.respond(&method, path, &h);
        let (parts, body) = resp.into_parts();
        let bytes = body.collect().await.expect("body").to_bytes().to_vec();
        (parts.status, parts.headers, bytes)
    }

    fn hv(h: &HeaderMap, name: header::HeaderName) -> &str {
        h.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default()
    }

    #[tokio::test]
    async fn client_routes_get_index_with_no_cache_and_csp() {
        let spa = fixture();
        for path in ["/", "/users", "/admin/traffic", "/index.html"] {
            let (status, h, body) = get(&spa, Method::GET, path, &[]).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(body, b"<!doctype html><title>rb</title>", "{path}");
            assert_eq!(hv(&h, header::CACHE_CONTROL), "no-cache");
            assert_eq!(hv(&h, header::CONTENT_TYPE), "text/html; charset=utf-8");
            assert_eq!(hv(&h, header::CONTENT_SECURITY_POLICY), CSP);
            assert!(hv(&h, header::ETAG).starts_with('"'));
        }
    }

    #[tokio::test]
    async fn hashed_assets_are_immutable_and_negotiate_precompressed_variants() {
        let spa = fixture();
        let (status, h, body) = get(&spa, Method::GET, "/assets/app-abc123.js", &[("accept-encoding", "gzip, deflate, br")]).await;
        assert_eq!((status, body.as_slice()), (StatusCode::OK, &b"BR"[..]));
        assert_eq!(hv(&h, header::CONTENT_ENCODING), "br");
        assert_eq!(hv(&h, header::CACHE_CONTROL), IMMUTABLE);
        assert_eq!(hv(&h, header::VARY), "accept-encoding");
        assert_eq!(hv(&h, header::CONTENT_TYPE), "text/javascript; charset=utf-8");
        assert!(h.get(header::CONTENT_SECURITY_POLICY).is_none());
        let br_etag = hv(&h, header::ETAG).to_string();

        let (_, h, body) = get(&spa, Method::GET, "/assets/app-abc123.js", &[("accept-encoding", "gzip, br;q=0")]).await;
        assert_eq!((body.as_slice(), hv(&h, header::CONTENT_ENCODING)), (&b"GZ"[..], "gzip"));
        assert_ne!(hv(&h, header::ETAG), br_etag, "each representation has its own etag");

        let (_, h, body) = get(&spa, Method::GET, "/assets/app-abc123.js", &[]).await;
        assert_eq!(body, b"console.log('app')");
        assert!(h.get(header::CONTENT_ENCODING).is_none());

        // No variants: no Vary.
        let (_, h, _) = get(&spa, Method::GET, "/assets/app-abc123.css", &[("accept-encoding", "br")]).await;
        assert!(h.get(header::VARY).is_none() && h.get(header::CONTENT_ENCODING).is_none());
    }

    #[tokio::test]
    async fn etags_answer_304() {
        let spa = fixture();
        let (_, h, _) = get(&spa, Method::GET, "/", &[]).await;
        let tag = hv(&h, header::ETAG).to_string();
        let (status, h, body) = get(&spa, Method::GET, "/users", &[("if-none-match", &format!("\"nope\", W/{tag}"))]).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        assert!(body.is_empty());
        assert_eq!(hv(&h, header::ETAG), tag);
        let (status, _, _) = get(&spa, Method::GET, "/", &[("if-none-match", "\"other\"")]).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn missing_assets_and_files_are_404_not_html() {
        let spa = fixture();
        for path in ["/assets/app-old999.js", "/assets/nested/route", "/missing.png", "/deep/robots.txt"] {
            let (status, h, _) = get(&spa, Method::GET, path, &[]).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(hv(&h, header::CONTENT_TYPE).starts_with("text/plain"));
        }
        let (status, h, body) = get(&spa, Method::GET, "/favicon.ico", &[]).await;
        assert_eq!((status, body.as_slice(), hv(&h, header::CACHE_CONTROL)), (StatusCode::OK, &b"ICO"[..], SHORT));
    }

    #[tokio::test]
    async fn methods_head_and_unbuilt_ui() {
        let spa = fixture();
        let (status, h, _) = get(&spa, Method::POST, "/", &[]).await;
        assert_eq!((status, hv(&h, header::ALLOW)), (StatusCode::METHOD_NOT_ALLOWED, "GET, HEAD"));
        let (status, h, body) = get(&spa, Method::HEAD, "/assets/app-abc123.js", &[]).await;
        assert_eq!((status, body.len(), hv(&h, header::CONTENT_LENGTH)), (StatusCode::OK, 0, "18"));

        let empty = Spa::from_files([]);
        assert!(!empty.is_built());
        let (status, _, body) = get(&empty, Method::GET, "/", &[]).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(String::from_utf8_lossy(&body).contains("web UI not built"));
    }

    #[tokio::test]
    async fn router_serves_through_axum() {
        let resp = fixture()
            .router()
            .oneshot(Request::builder().uri("/users?pageSize=5").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get(header::CACHE_CONTROL).map(|v| v.as_bytes()), Some(&b"no-cache"[..]));
    }

    #[test]
    fn content_types_cover_vite_output() {
        assert_eq!(content_type("a/b.MJS"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("x.woff2"), "font/woff2");
        assert_eq!(content_type("noext"), "application/octet-stream");
    }
}
