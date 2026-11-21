//! Clients for the rb server (or anything else speaking HTTP/WebSocket).
//!
//! [`HttpClient`] is a pooled hyper client whose connector does TCP + rustls itself, so the
//! whole TLS stack stays on `ring` and HTTP/2 is negotiated by ALPN (or forced with h2c prior
//! knowledge). [`ws`] connects WebSockets over the same stream type, and [`loadgen`] drives
//! concurrent load and reports latency percentiles live.

mod error;
mod http_client;
pub mod loadgen;
mod stream;
pub mod ws;

pub use error::Error;
pub use http_client::{ClientOptions, HttpClient, Response};
pub use stream::Stream;

use std::time::Duration;

/// [`rb_core::retry::Sleeper`] on the tokio timer.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioSleeper;

impl rb_core::retry::Sleeper for TokioSleeper {
    fn sleep(&self, d: Duration) -> impl std::future::Future<Output = ()> + Send {
        tokio::time::sleep(d)
    }
}
