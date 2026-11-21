//! Health probes. Probes run in parallel with a per-probe timeout, so a report costs the slowest
//! probe, not the sum.

use serde::Serialize;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub type ProbeFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

pub trait Probe: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn check(&self) -> ProbeFuture<'_>;
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub name: String,
    pub healthy: bool,
    pub latency_ns: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
}

pub(crate) async fn run_all(probes: &[Arc<dyn Probe>], timeout: Duration) -> Vec<ProbeResult> {
    futures_util::future::join_all(probes.iter().map(|p| async move {
        let start = Instant::now();
        let outcome = match tokio::time::timeout(timeout, p.check()).await {
            Ok(r) => r,
            Err(_) => Err(format!("timed out after {timeout:?}")),
        };
        ProbeResult {
            name: p.name().to_string(),
            healthy: outcome.is_ok(),
            latency_ns: u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX),
            err: outcome.err(),
        }
    }))
    .await
}
