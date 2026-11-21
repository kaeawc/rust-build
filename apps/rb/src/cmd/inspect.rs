//! `rb inspect`: an event-bus demo as a plain streaming report (port of golang-build's
//! `cmd/inspect`). Synthetic producers publish to an async bus; a subscriber keeps per-level and
//! per-kind counters and a ring of recent events, printed once per interval.

use rb_core::eventbus::{Async, AsyncConfig};
use rb_core::random::{Pcg32, Random as _};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Scenario {
    /// ~200 events/s, mostly info.
    Steady,
    /// Quiet with periodic bursts.
    Bursty,
    /// Many warnings and errors.
    Noisy,
}

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, value_enum, default_value = "steady")]
    scenario: Scenario,
    #[arg(long, value_parser = super::duration, default_value = "5s")]
    duration: Duration,
    #[arg(long, value_parser = super::duration, default_value = "1s")]
    interval: Duration,
    /// Recent events to print at the end.
    #[arg(long, default_value_t = 5)]
    tail: usize,
}

#[derive(Clone, Debug)]
struct Event {
    level: &'static str,
    kind: &'static str,
    message: String,
}

#[derive(Default)]
struct Inspector {
    total: u64,
    by_level: BTreeMap<&'static str, u64>,
    by_kind: BTreeMap<&'static str, u64>,
    recent: VecDeque<Event>,
}

impl Inspector {
    fn observe(&mut self, e: Event, keep: usize) {
        self.total += 1;
        *self.by_level.entry(e.level).or_default() += 1;
        *self.by_kind.entry(e.kind).or_default() += 1;
        self.recent.push_back(e);
        while self.recent.len() > keep.max(1) {
            self.recent.pop_front();
        }
    }
}

const KINDS: [&str; 4] = ["http", "db", "cache", "job"];

fn next_event(r: &mut Pcg32, scenario: Scenario, n: u64) -> Event {
    let roll = r.next_f64();
    let (warn, err) = match scenario {
        Scenario::Noisy => (0.3, 0.15),
        _ => (0.05, 0.01),
    };
    let level = if roll < err {
        "error"
    } else if roll < err + warn {
        "warn"
    } else {
        "info"
    };
    let kind = KINDS[r.below(KINDS.len() as u64) as usize];
    Event { level, kind, message: format!("{kind} event #{n}") }
}

fn per_tick(scenario: Scenario, tick: u64) -> u64 {
    match scenario {
        Scenario::Steady | Scenario::Noisy => 4,
        Scenario::Bursty if tick % 50 < 5 => 60,
        Scenario::Bursty => 0,
    }
}

pub async fn run(a: Args) -> anyhow::Result<()> {
    let bus = Arc::new(Async::<Event>::new(AsyncConfig { buffer: 4096, drop_when_full: true }));
    let state = Arc::new(Mutex::new(Inspector::default()));
    let (s, keep) = (state.clone(), a.tail);
    let _sub = bus.subscribe(move |e| rb_core::clock::lock(&s).observe(e, keep));

    let producer = {
        let bus = bus.clone();
        let deadline = Instant::now() + a.duration;
        tokio::spawn(async move {
            let mut r = Pcg32::from_entropy();
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            let (mut n, mut t) = (0u64, 0u64);
            while Instant::now() < deadline {
                tick.tick().await;
                for _ in 0..per_tick(a.scenario, t) {
                    n += 1;
                    bus.publish(next_event(&mut r, a.scenario, n));
                }
                t += 1;
            }
        })
    };

    let start = Instant::now();
    let mut report = tokio::time::interval(a.interval);
    report.tick().await;
    let mut last = 0;
    loop {
        tokio::select! {
            _ = report.tick() => {
                let st = rb_core::clock::lock(&state);
                let rate = (st.total - last) as f64 / a.interval.as_secs_f64();
                last = st.total;
                let levels: Vec<String> = st.by_level.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("{:>5.1}s  total {:>6}  {:>7.1}/s  {}", start.elapsed().as_secs_f64(), st.total, rate, levels.join(" "));
            }
            _ = tokio::time::sleep(a.duration.saturating_sub(start.elapsed())), if producer.is_finished() => break,
        }
        if producer.is_finished() {
            break;
        }
    }
    let _ = producer.await;
    bus.close();
    let st = rb_core::clock::lock(&state);
    let kinds: Vec<String> = st.by_kind.iter().map(|(k, v)| format!("{k}={v}")).collect();
    println!("done: {} events ({} dropped)  {}", st.total, bus.dropped(), kinds.join(" "));
    for e in &st.recent {
        println!("  [{:<5}] {:<5} {}", e.level, e.kind, e.message);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspector_counts_and_keeps_a_bounded_tail() {
        let mut i = Inspector::default();
        let mut r = Pcg32::seeded(1);
        for n in 0..100 {
            i.observe(next_event(&mut r, Scenario::Noisy, n), 3);
        }
        assert_eq!(i.total, 100);
        assert_eq!(i.by_level.values().sum::<u64>(), 100);
        assert_eq!(i.recent.len(), 3);
        assert!(i.by_level.get("error").copied().unwrap_or(0) > 0, "noisy produces errors");
        assert_eq!(per_tick(Scenario::Bursty, 1), 60);
        assert_eq!(per_tick(Scenario::Bursty, 10), 0);
    }
}
