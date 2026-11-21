use crate::Error;
use crate::stream::{Conn, Stream, target};
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, Uri, Version, header};
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use rb_tls::rustls::ClientConfig;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

#[derive(Clone)]
pub struct ClientOptions {
    pub tls: rb_tls::ClientOptions,
    /// Plaintext HTTP/2 with prior knowledge (h2c). Over TLS, HTTP/2 is negotiated by ALPN.
    pub h2c: bool,
    /// Only offer `h2` in ALPN (fail rather than fall back to HTTP/1.1).
    pub http2_only: bool,
    /// Only offer `http/1.1` in ALPN.
    pub http1_only: bool,
    pub timeout: Duration,
    pub pool_max_idle_per_host: usize,
}

impl Default for ClientOptions {
    fn default() -> Self {
        ClientOptions {
            tls: rb_tls::ClientOptions::default(),
            h2c: false,
            http2_only: false,
            http1_only: false,
            timeout: Duration::from_secs(30),
            pool_max_idle_per_host: 64,
        }
    }
}

#[derive(Clone)]
struct Connector {
    tls: Arc<ClientConfig>,
}

impl tower::Service<Uri> for Connector {
    type Response = Conn;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Conn, Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let tls = self.tls.clone();
        Box::pin(async move {
            let (secure, host, port) = target(&uri, &["https"], &["http"])?;
            let stream = Stream::connect(&host, port, secure.then_some(tls)).await?;
            Ok(Conn(TokioIo::new(stream)))
        })
    }
}

/// A fully-read response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: StatusCode,
    pub version: Version,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T, Error> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    /// Errors on non-2xx statuses, keeping the body for the message.
    pub fn error_for_status(self) -> Result<Self, Error> {
        if self.status.is_success() { Ok(self) } else { Err(Error::Status { status: self.status.as_u16(), body: self.text() }) }
    }
}

/// Pooled HTTP/1.1 + HTTP/2 client. Cheap to clone; clones share the pool.
#[derive(Clone)]
pub struct HttpClient {
    inner: Client<Connector, Full<Bytes>>,
    timeout: Duration,
}

impl HttpClient {
    pub fn new(mut opts: ClientOptions) -> Result<Self, Error> {
        opts.tls.alpn = match (opts.http2_only, opts.http1_only) {
            (true, _) => vec![rb_tls::ALPN_H2.to_vec()],
            (_, true) => vec![rb_tls::ALPN_HTTP11.to_vec()],
            _ => vec![rb_tls::ALPN_H2.to_vec(), rb_tls::ALPN_HTTP11.to_vec()],
        };
        let tls = rb_tls::client_config(opts.tls)?;
        let inner = Client::builder(TokioExecutor::new())
            .timer(TokioTimer::new())
            .pool_timer(TokioTimer::new())
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(opts.pool_max_idle_per_host)
            .http2_only(opts.h2c)
            .build(Connector { tls });
        Ok(HttpClient { inner, timeout: opts.timeout })
    }

    pub async fn send(
        &self,
        method: Method,
        url: &str,
        headers: &[(HeaderName, HeaderValue)],
        body: Option<Bytes>,
    ) -> Result<Response, Error> {
        let uri: Uri = url.parse().map_err(|e: http::uri::InvalidUri| Error::Url(url.to_string(), e.to_string()))?;
        let mut req = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let req = req.body(Full::new(body.unwrap_or_default()))?;
        let fut = async {
            let resp = self.inner.request(req).await?;
            let (parts, body) = resp.into_parts();
            let body = body.collect().await?.to_bytes();
            Ok::<_, Error>(Response { status: parts.status, version: parts.version, headers: parts.headers, body })
        };
        tokio::time::timeout(self.timeout, fut).await.map_err(|_| Error::Timeout(self.timeout))?
    }

    pub async fn get(&self, url: &str) -> Result<Response, Error> {
        self.send(Method::GET, url, &[], None).await
    }

    pub async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, Error> {
        self.get(url).await?.error_for_status()?.json()
    }

    pub async fn post_json<B: Serialize, T: DeserializeOwned>(&self, url: &str, body: &B) -> Result<T, Error> {
        let json = Bytes::from(serde_json::to_vec(body)?);
        let ct = [(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))];
        self.send(Method::POST, url, &ct, Some(json)).await?.error_for_status()?.json()
    }
}
