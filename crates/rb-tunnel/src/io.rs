//! Byte relaying with an optional idle timeout.

use crate::Stats;
use rb_core::clock::SharedClock;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Any bidirectional byte stream a tunnel can relay.
pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}

pub type BoxStream = Box<dyn Stream>;

/// Tracks the last time bytes moved, against a [`Clock`], so the idle rule is testable without
/// waiting.
#[derive(Clone)]
pub struct IdleTracker {
    clock: SharedClock,
    timeout: Duration,
    last: Arc<Mutex<Instant>>,
}

impl IdleTracker {
    pub fn new(clock: SharedClock, timeout: Duration) -> Self {
        let now = clock.now();
        IdleTracker { clock, timeout, last: Arc::new(Mutex::new(now)) }
    }

    pub fn touch(&self) {
        *rb_core::clock::lock(&self.last) = self.clock.now();
    }

    /// Time left before the stream counts as idle; `None` once it is.
    pub fn remaining(&self) -> Option<Duration> {
        let idle_for = self.clock.now().saturating_duration_since(*rb_core::clock::lock(&self.last));
        self.timeout.checked_sub(idle_for).filter(|d| !d.is_zero())
    }

    /// Resolves once nothing has moved for the whole timeout.
    async fn expired(&self) {
        while let Some(left) = self.remaining() {
            tokio::time::sleep(left).await;
        }
    }
}

/// Wraps a stream and touches the tracker whenever bytes are read or written.
struct Touching<S> {
    inner: S,
    idle: IdleTracker,
}

impl<S: AsyncRead + Unpin> AsyncRead for Touching<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if buf.filled().len() > before {
            self.idle.touch();
        }
        r
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Touching<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write(cx, buf);
        if matches!(r, Poll::Ready(Ok(n)) if n > 0) {
            self.idle.touch();
        }
        r
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Relays bytes both ways until both sides close, recording byte counts in `stats`. With an
/// idle timeout, a stream that moves nothing for that long is closed (not counted as a failure).
pub async fn pipe<A, B>(a: A, mut b: B, stats: &Stats, idle: Option<Duration>) -> Result<(), String>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let result = match idle {
        None => {
            let mut a = a;
            tokio::io::copy_bidirectional(&mut a, &mut b).await
        }
        Some(timeout) => {
            let tracker = IdleTracker::new(Arc::new(rb_core::clock::System), timeout);
            let mut a = Touching { inner: a, idle: tracker.clone() };
            tokio::select! {
                r = tokio::io::copy_bidirectional(&mut a, &mut b) => r,
                () = tracker.expired() => {
                    tracing::debug!(?timeout, "closing idle tunnel stream");
                    stats.record_idle_close();
                    return Ok(());
                }
            }
        }
    };
    match result {
        Ok((up, down)) => {
            stats.record_bytes(up, down);
            Ok(())
        }
        // Peers that drop without close_notify are routine for tunnels; not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rb_core::clock::Fake;

    #[test]
    fn idle_tracker_expires_only_after_a_full_quiet_timeout() {
        let clock = Fake::default();
        let t = IdleTracker::new(Arc::new(clock.clone()), Duration::from_secs(30));
        assert_eq!(t.remaining(), Some(Duration::from_secs(30)));
        clock.advance(Duration::from_secs(20));
        assert_eq!(t.remaining(), Some(Duration::from_secs(10)));
        t.touch();
        clock.advance(Duration::from_secs(29));
        assert_eq!(t.remaining(), Some(Duration::from_secs(1)));
        clock.advance(Duration::from_secs(1));
        assert_eq!(t.remaining(), None);
    }

    #[tokio::test]
    async fn touching_records_reads_and_writes() {
        let clock = Fake::default();
        let t = IdleTracker::new(Arc::new(clock.clone()), Duration::from_secs(5));
        let (a, mut b) = tokio::io::duplex(64);
        let mut s = Touching { inner: a, idle: t.clone() };
        clock.advance(Duration::from_secs(4));
        tokio::io::AsyncWriteExt::write_all(&mut s, b"x").await.expect("write");
        assert_eq!(t.remaining(), Some(Duration::from_secs(5)));
        clock.advance(Duration::from_secs(4));
        tokio::io::AsyncWriteExt::write_all(&mut b, b"y").await.expect("write");
        let mut buf = [0u8; 1];
        tokio::io::AsyncReadExt::read_exact(&mut s, &mut buf).await.expect("read");
        assert_eq!(t.remaining(), Some(Duration::from_secs(5)));
    }
}
