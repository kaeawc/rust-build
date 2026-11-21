//! WebSocket client over [`Stream`] (`ws://` and `wss://`).

use crate::Error;
use crate::stream::{Stream, target};
use futures_util::{SinkExt, StreamExt};
use std::time::{Duration, Instant};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

pub type Socket = WebSocketStream<Stream>;

/// Connects to a `ws://` or `wss://` URL. The TLS config's ALPN is forced to `http/1.1`, since
/// the upgrade handshake is HTTP/1.1.
pub async fn connect(url: &str, mut tls: rb_tls::ClientOptions) -> Result<Socket, Error> {
    let uri: http::Uri = url.parse().map_err(|e: http::uri::InvalidUri| Error::Url(url.to_string(), e.to_string()))?;
    let (secure, host, port) = target(&uri, &["wss", "https"], &["ws", "http"])?;
    let cfg = if secure {
        tls.alpn = vec![rb_tls::ALPN_HTTP11.to_vec()];
        Some(rb_tls::client_config(tls)?)
    } else {
        None
    };
    let stream = Stream::connect(&host, port, cfg).await?;
    let (socket, _resp) = tokio_tungstenite::client_async(url, stream).await?;
    Ok(socket)
}

/// Sends `text` and waits for the next text/binary frame; returns it with the round-trip time.
pub async fn round_trip(socket: &mut Socket, text: &str) -> Result<(String, Duration), Error> {
    let start = Instant::now();
    socket.send(Message::text(text)).await?;
    while let Some(msg) = socket.next().await {
        match msg? {
            Message::Text(t) => return Ok((t.to_string(), start.elapsed())),
            Message::Binary(b) => return Ok((String::from_utf8_lossy(&b).into_owned(), start.elapsed())),
            Message::Close(_) => break,
            _ => {}
        }
    }
    Err(Error::WebSocket(tokio_tungstenite::tungstenite::Error::ConnectionClosed))
}
