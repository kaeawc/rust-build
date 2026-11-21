use crate::tlsargs::ClientTls;
use rb_client::loadgen::{self, HttpTarget, Snapshot};
use std::sync::Arc;
use std::time::Duration;

#[derive(clap::Args)]
pub struct Args {
    url: String,
    /// Concurrent workers.
    #[arg(short, long, default_value_t = 16)]
    concurrency: usize,
    /// How long to run.
    #[arg(short, long, value_parser = super::duration, default_value = "10s")]
    duration: Duration,
    /// Stop after this many requests.
    #[arg(short = 'n', long)]
    requests: Option<u64>,
    /// How often to print a progress line.
    #[arg(long, value_parser = super::duration, default_value = "1s")]
    interval: Duration,
    /// Plaintext HTTP/2 with prior knowledge (h2c).
    #[arg(long)]
    h2c: bool,
    /// Force HTTP/1.1 (one connection per worker instead of h2 multiplexing).
    #[arg(long)]
    http1: bool,
    /// Print the final summary as JSON.
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    tls: ClientTls,
}

fn line(s: &Snapshot) -> String {
    format!(
        "{:>6.1}s  {:>8} req  {:>9.1} rps  p50 {:>9.2?}  p95 {:>9.2?}  p99 {:>9.2?}  err {}",
        s.elapsed.as_secs_f64(),
        s.total,
        s.rps,
        s.p50,
        s.p95,
        s.p99,
        s.errors
    )
}

pub async fn run(a: Args) -> anyhow::Result<()> {
    let client = rb_client::HttpClient::new(rb_client::ClientOptions {
        tls: a.tls.options()?,
        h2c: a.h2c,
        http1_only: a.http1,
        pool_max_idle_per_host: a.concurrency.max(1),
        ..Default::default()
    })?;
    eprintln!("loadgen {} — {} workers for {:?}", a.url, a.concurrency, a.duration);
    let run = loadgen::start(
        Arc::new(HttpTarget { client, url: a.url.clone() }),
        loadgen::Config { concurrency: a.concurrency, duration: a.duration, max_requests: a.requests },
    );
    let mut tick = tokio::time::interval(a.interval);
    tick.tick().await;
    let finished = loop {
        tokio::select! {
            s = run.wait() => break s,
            _ = tick.tick() => eprintln!("{}", line(&run.snapshot())),
            _ = tokio::signal::ctrl_c() => { run.stop(); break run.wait().await; }
        }
    };
    if a.json {
        let statuses: serde_json::Map<String, serde_json::Value> =
            finished.statuses.iter().map(|(k, v)| (k.to_string(), (*v).into())).collect();
        println!(
            "{}",
            serde_json::json!({
                "url": a.url, "elapsedSecs": finished.elapsed.as_secs_f64(), "total": finished.total, "errors": finished.errors,
                "rps": finished.rps, "p50Ms": finished.p50.as_secs_f64() * 1e3, "p95Ms": finished.p95.as_secs_f64() * 1e3,
                "p99Ms": finished.p99.as_secs_f64() * 1e3, "statuses": statuses, "lastError": finished.last_error,
            })
        );
    } else {
        println!("{}", line(&finished));
        for (status, n) in &finished.statuses {
            println!("  {status}: {n}");
        }
        if let Some(e) = &finished.last_error {
            println!("  last error: {e}");
        }
    }
    Ok(())
}
