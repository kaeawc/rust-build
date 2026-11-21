//! Dependency-light building blocks shared by the server, client, CLI and desktop app.
//!
//! Everything that reads time takes a [`clock::Clock`] so tests drive it with [`clock::Fake`]
//! instead of sleeping. Nothing here depends on an async runtime.

pub mod cache;
pub mod circuitbreaker;
pub mod clock;
pub mod eventbus;
pub mod idgen;
pub mod kv;
pub mod paginator;
pub mod random;
pub mod ratelimit;
pub mod retry;
pub mod stats;

#[cfg(test)]
pub(crate) mod testutil {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    /// Polls a future that never really waits (fake sleepers, ready futures) to completion.
    pub fn block_on<F: Future>(f: F) -> F::Output {
        let mut f = std::pin::pin!(f);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }
}
