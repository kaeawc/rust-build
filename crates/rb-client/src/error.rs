#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid url {0:?}: {1}")]
    Url(String, String),
    #[error("connect: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls: {0}")]
    Tls(#[from] rb_tls::Error),
    #[error("request: {0}")]
    Request(#[from] hyper_util::client::legacy::Error),
    #[error("reading body: {0}")]
    Body(#[from] hyper::Error),
    #[error("building request: {0}")]
    Http(#[from] http::Error),
    #[error("timed out after {0:?}")]
    Timeout(std::time::Duration),
    #[error("unexpected status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("websocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
}
