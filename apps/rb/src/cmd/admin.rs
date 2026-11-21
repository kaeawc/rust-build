//! `rb admin`: inspect a running deployment over its API (port of golang-build's admin TUI,
//! as plain commands).

use crate::tlsargs::ClientTls;
use serde_json::Value;

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    action: Action,
    /// Server base URL.
    #[arg(long, global = true, env = "RB_URL", default_value = "http://127.0.0.1:8080")]
    url: String,
    /// Print raw JSON instead of a table.
    #[arg(long, global = true)]
    json: bool,
    #[command(flatten)]
    tls: ClientTls,
}

#[derive(clap::Subcommand)]
enum Action {
    /// List users (all pages).
    Users,
    /// Run the server's health probes.
    Health,
    /// Per-route request counts and latency.
    Traffic,
    /// Liveness, readiness and protocol in one view.
    Status,
}

pub async fn run(a: Args) -> anyhow::Result<()> {
    let client = rb_client::HttpClient::new(rb_client::ClientOptions { tls: a.tls.options()?, ..Default::default() })?;
    let base = a.url.trim_end_matches('/');
    match a.action {
        Action::Users => {
            let mut all = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let url = match &cursor {
                    Some(c) => format!("{base}/api/users?pageSize=100&cursor={c}"),
                    None => format!("{base}/api/users?pageSize=100"),
                };
                let page: Value = client.get_json(&url).await?;
                all.extend(page["items"].as_array().cloned().unwrap_or_default());
                match page["nextCursor"].as_str() {
                    Some(c) => cursor = Some(c.to_string()),
                    None => break,
                }
            }
            if a.json {
                println!("{}", serde_json::to_string_pretty(&all)?);
            } else {
                println!("{:>8}  NAME", "ID");
                for u in &all {
                    println!("{:>8}  {}", u["id"], u["name"].as_str().unwrap_or_default());
                }
                println!("{} users", all.len());
            }
        }
        Action::Health => {
            let probes: Vec<Value> = client.get_json(&format!("{base}/api/admin/healthchecks")).await?;
            if a.json {
                println!("{}", serde_json::to_string_pretty(&probes)?);
            } else {
                for p in &probes {
                    let ok = p["healthy"].as_bool().unwrap_or(false);
                    let ms = p["latencyNs"].as_f64().unwrap_or(0.0) / 1e6;
                    println!(
                        "{} {:<10} {ms:>8.2} ms  {}",
                        if ok { "ok  " } else { "FAIL" },
                        p["name"].as_str().unwrap_or("?"),
                        p["err"].as_str().unwrap_or("")
                    );
                }
                if probes.iter().any(|p| p["healthy"] != true) {
                    std::process::exit(1);
                }
            }
        }
        Action::Traffic => {
            let rows: Vec<Value> = client.get_json(&format!("{base}/api/admin/traffic")).await?;
            if a.json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!("{:<7} {:<28} {:>8} {:>6} {:>6} {:>6} {:>10}", "METHOD", "ROUTE", "COUNT", "2XX", "4XX", "5XX", "AVG");
                for r in &rows {
                    let count = r["count"].as_f64().unwrap_or(0.0);
                    let avg = if count > 0.0 { r["totalNanos"].as_f64().unwrap_or(0.0) / count / 1e6 } else { 0.0 };
                    println!(
                        "{:<7} {:<28} {:>8} {:>6} {:>6} {:>6} {:>7.2} ms",
                        r["method"].as_str().unwrap_or(""),
                        r["path"].as_str().unwrap_or(""),
                        r["count"],
                        r["status2xx"],
                        r["status4xx"],
                        r["status5xx"],
                        avg
                    );
                }
            }
        }
        Action::Status => {
            let live = client.get(&format!("{base}/healthz")).await?;
            let ready = client.get(&format!("{base}/readyz")).await?;
            let info: Value = client.get_json(&format!("{base}/api/info")).await?;
            println!("liveness   {}", live.status);
            println!("readiness  {}", ready.status);
            println!("protocol   {}", info["httpVersion"].as_str().unwrap_or("?"));
            println!("version    {}", info["version"].as_str().unwrap_or("?"));
            println!("uptime     {}s", info["uptimeSecs"]);
        }
    }
    Ok(())
}
