//! The API server: a port of golang-build's `cmd/server` onto axum + hyper.
//!
//! [`router`] builds the app; [`serve`] runs it on any listener over HTTP/1.1 and HTTP/2
//! (negotiated by ALPN under TLS, or detected from the h2c preface in plaintext), with WebSocket
//! upgrades on both (RFC 8441 extended CONNECT on HTTP/2) and bounded graceful shutdown.

mod api;
mod error;
mod health;
mod serve;
mod state;
mod store;
mod traffic;

pub use api::router;
pub use error::ApiError;
pub use health::{Probe, ProbeFuture, ProbeResult};
pub use serve::{ServeOptions, serve, shutdown_signal};
pub use state::{AppState, Config};
pub use store::{BoxFut, MemoryStore, NewUser, StoreError, User, UserStore};
pub use traffic::{TrafficRecorder, TrafficStat};
