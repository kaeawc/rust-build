use crate::tlsargs::ClientTls;
use anyhow::Context as _;
use http::{HeaderName, HeaderValue, Method};
use std::io::Write as _;
use std::time::{Duration, Instant};

#[derive(clap::Args)]
pub struct Args {
    url: String,
    /// HTTP method.
    #[arg(short = 'X', long, default_value = "GET")]
    method: String,
    /// Request header, `Name: value` (repeatable).
    #[arg(short = 'H', long = "header")]
    headers: Vec<String>,
    /// Request body (sent with `content-type: application/json` unless -H overrides).
    #[arg(short, long)]
    data: Option<String>,
    /// Print status line and response headers.
    #[arg(short, long)]
    include: bool,
    /// Plaintext HTTP/2 with prior knowledge (h2c).
    #[arg(long, conflicts_with = "http1")]
    h2c: bool,
    /// Require HTTP/2 over TLS.
    #[arg(long, conflicts_with = "http1")]
    http2: bool,
    /// Force HTTP/1.1.
    #[arg(long)]
    http1: bool,
    #[arg(long, value_parser = super::duration, default_value = "30s")]
    timeout: Duration,
    #[command(flatten)]
    tls: ClientTls,
}

pub async fn run(a: Args) -> anyhow::Result<()> {
    let client = rb_client::HttpClient::new(rb_client::ClientOptions {
        tls: a.tls.options()?,
        h2c: a.h2c,
        http2_only: a.http2,
        http1_only: a.http1,
        timeout: a.timeout,
        ..Default::default()
    })?;
    let mut headers = Vec::new();
    for h in &a.headers {
        let (k, v) = h.split_once(':').with_context(|| format!("header {h:?} must be `Name: value`"))?;
        headers.push((HeaderName::try_from(k.trim())?, HeaderValue::try_from(v.trim())?));
    }
    if a.data.is_some() && !headers.iter().any(|(k, _)| k == http::header::CONTENT_TYPE) {
        headers.push((http::header::CONTENT_TYPE, HeaderValue::from_static("application/json")));
    }
    let method = Method::from_bytes(a.method.to_uppercase().as_bytes())?;
    let start = Instant::now();
    let resp = client.send(method, &a.url, &headers, a.data.map(bytes::Bytes::from)).await?;
    let elapsed = start.elapsed();
    let mut out = std::io::stdout().lock();
    if a.include {
        writeln!(out, "{:?} {}", resp.version, resp.status)?;
        for (k, v) in &resp.headers {
            writeln!(out, "{k}: {}", v.to_str().unwrap_or("<binary>"))?;
        }
        writeln!(out)?;
    }
    out.write_all(&resp.body)?;
    if !resp.body.ends_with(b"\n") {
        writeln!(out)?;
    }
    tracing::debug!(?elapsed, version = ?resp.version, "done");
    if !resp.status.is_success() {
        std::process::exit(22); // like curl --fail
    }
    Ok(())
}
