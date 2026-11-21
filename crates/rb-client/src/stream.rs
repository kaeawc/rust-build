use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::TokioIo;
use rb_tls::rustls::ClientConfig;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// A plaintext or TLS client connection.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl Stream {
    /// Connects to `host:port`, then runs a TLS handshake when `tls` is given.
    pub async fn connect(host: &str, port: u16, tls: Option<Arc<ClientConfig>>) -> Result<Stream, crate::Error> {
        let tcp = TcpStream::connect((host.trim_start_matches('[').trim_end_matches(']'), port)).await?;
        let _ = tcp.set_nodelay(true);
        match tls {
            None => Ok(Stream::Plain(tcp)),
            Some(cfg) => {
                let name = rb_tls::server_name(host)?;
                let tls = TlsConnector::from(cfg).connect(name, tcp).await?;
                Ok(Stream::Tls(Box::new(tls)))
            }
        }
    }

    /// Whether ALPN settled on HTTP/2.
    pub fn is_h2(&self) -> bool {
        match self {
            Stream::Plain(_) => false,
            Stream::Tls(t) => t.get_ref().1.alpn_protocol() == Some(rb_tls::ALPN_H2),
        }
    }
}

impl AsyncRead for Stream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }
    fn poll_write_vectored(self: Pin<&mut Self>, cx: &mut Context<'_>, bufs: &[io::IoSlice<'_>]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write_vectored(cx, bufs),
        }
    }
    fn is_write_vectored(&self) -> bool {
        match self {
            Stream::Plain(s) => s.is_write_vectored(),
            Stream::Tls(s) => s.is_write_vectored(),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// A [`Stream`] adapted to hyper's IO traits, reporting ALPN to the connection pool.
pub(crate) struct Conn(pub(crate) TokioIo<Stream>);

impl Connection for Conn {
    fn connected(&self) -> Connected {
        let c = Connected::new();
        if self.0.inner().is_h2() { c.negotiated_h2() } else { c }
    }
}

impl hyper::rt::Read for Conn {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: hyper::rt::ReadBufCursor<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for Conn {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
    fn is_write_vectored(&self) -> bool {
        self.0.is_write_vectored()
    }
    fn poll_write_vectored(self: Pin<&mut Self>, cx: &mut Context<'_>, bufs: &[io::IoSlice<'_>]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_write_vectored(cx, bufs)
    }
}

/// Splits a URI into (secure, host, port), with scheme default ports.
pub(crate) fn target(uri: &http::Uri, secure_schemes: &[&str], plain_schemes: &[&str]) -> Result<(bool, String, u16), crate::Error> {
    let bad = |why: &str| crate::Error::Url(uri.to_string(), why.to_string());
    let scheme = uri.scheme_str().ok_or_else(|| bad("missing scheme"))?;
    let secure = if secure_schemes.contains(&scheme) {
        true
    } else if plain_schemes.contains(&scheme) {
        false
    } else {
        return Err(bad("unsupported scheme"));
    };
    let host = uri.host().ok_or_else(|| bad("missing host"))?.to_string();
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    Ok((secure, host, port))
}
