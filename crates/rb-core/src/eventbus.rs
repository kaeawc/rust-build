//! In-process pub/sub.
//!
//! [`Sync`] delivers on the publisher's thread in subscription order. [`Async`] gives each
//! subscriber its own bounded queue and worker thread, so a slow listener never blocks the
//! publisher or other listeners; with `drop_when_full` a full queue drops instead of waiting.

use crate::clock::lock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

type Handler<E> = Arc<dyn Fn(&E) + Send + std::marker::Sync>;
type Subscribers<E> = Arc<Mutex<Vec<(u64, Handler<E>)>>>;

/// Removes a subscription when called; calling it again does nothing.
pub struct Unsubscribe(Option<Box<dyn FnOnce() + Send>>);

impl Unsubscribe {
    pub fn unsubscribe(mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

pub struct Sync<E> {
    subs: Subscribers<E>,
    next: AtomicU64,
}

impl<E: 'static> Default for Sync<E> {
    fn default() -> Self {
        Sync { subs: Arc::default(), next: AtomicU64::new(0) }
    }
}

impl<E: 'static> Sync<E> {
    pub fn subscribe(&self, f: impl Fn(&E) + Send + std::marker::Sync + 'static) -> Unsubscribe {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        lock(&self.subs).push((id, Arc::new(f)));
        let subs = Arc::downgrade(&self.subs);
        Unsubscribe(Some(Box::new(move || {
            if let Some(subs) = subs.upgrade() {
                lock(&subs).retain(|(i, _)| *i != id);
            }
        })))
    }

    pub fn publish(&self, e: &E) {
        // Snapshot so handlers may (un)subscribe without deadlocking.
        let handlers: Vec<Handler<E>> = lock(&self.subs).iter().map(|(_, h)| h.clone()).collect();
        for h in handlers {
            h(e);
        }
    }

    pub fn subscribers(&self) -> usize {
        lock(&self.subs).len()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AsyncConfig {
    pub buffer: usize,
    pub drop_when_full: bool,
}

impl Default for AsyncConfig {
    fn default() -> Self {
        AsyncConfig { buffer: 256, drop_when_full: false }
    }
}

struct Sub<E> {
    id: u64,
    tx: SyncSender<E>,
}

pub struct Async<E> {
    cfg: AsyncConfig,
    subs: Arc<Mutex<Vec<Sub<E>>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    next: AtomicU64,
    dropped: AtomicU64,
}

impl<E: Clone + Send + 'static> Async<E> {
    pub fn new(cfg: AsyncConfig) -> Self {
        Async { cfg, subs: Arc::default(), workers: Mutex::default(), next: AtomicU64::new(0), dropped: AtomicU64::new(0) }
    }

    pub fn subscribe(&self, f: impl Fn(E) + Send + 'static) -> Unsubscribe {
        let (tx, rx) = mpsc::sync_channel::<E>(self.cfg.buffer.max(1));
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        lock(&self.subs).push(Sub { id, tx });
        lock(&self.workers).push(std::thread::spawn(move || {
            for e in rx {
                f(e);
            }
        }));
        let subs = Arc::downgrade(&self.subs);
        Unsubscribe(Some(Box::new(move || {
            if let Some(subs) = subs.upgrade() {
                lock(&subs).retain(|s| s.id != id);
            }
        })))
    }

    pub fn publish(&self, e: E) {
        let senders: Vec<SyncSender<E>> = lock(&self.subs).iter().map(|s| s.tx.clone()).collect();
        for tx in senders {
            if self.cfg.drop_when_full {
                if let Err(TrySendError::Full(_)) = tx.try_send(e.clone()) {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                let _ = tx.send(e.clone());
            }
        }
    }

    /// Events dropped because a subscriber's queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Stops accepting events, drains every queue and waits for the workers.
    pub fn close(&self) {
        lock(&self.subs).clear();
        let workers = std::mem::take(&mut *lock(&self.workers));
        for w in workers {
            let _ = w.join();
        }
    }
}

impl<E> Drop for Async<E> {
    fn drop(&mut self) {
        lock(&self.subs).clear();
        for w in std::mem::take(&mut *lock(&self.workers)) {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn sync_delivers_in_order_and_unsubscribes() {
        let bus = Sync::<u32>::default();
        let log = Arc::new(Mutex::new(Vec::new()));
        let (a, b) = (log.clone(), log.clone());
        let ua = bus.subscribe(move |e| lock(&a).push(("a", *e)));
        let _ub = bus.subscribe(move |e| lock(&b).push(("b", *e)));
        bus.publish(&1);
        ua.unsubscribe();
        bus.publish(&2);
        assert_eq!(*lock(&log), vec![("a", 1), ("b", 1), ("b", 2)]);
        assert_eq!(bus.subscribers(), 1);
    }

    #[test]
    fn async_drains_on_close() {
        let bus = Async::<u32>::new(AsyncConfig::default());
        let sum = Arc::new(AtomicUsize::new(0));
        let s = sum.clone();
        let _u = bus.subscribe(move |e| {
            s.fetch_add(e as usize, Ordering::Relaxed);
        });
        for i in 1..=100 {
            bus.publish(i);
        }
        bus.close();
        assert_eq!(sum.load(Ordering::Relaxed), 5050);
    }

    #[test]
    fn async_drop_when_full_counts_drops() {
        let bus = Async::<u32>::new(AsyncConfig { buffer: 1, drop_when_full: true });
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate = Mutex::new(gate_rx);
        let _u = bus.subscribe(move |_| {
            let _ = lock(&gate).recv();
        });
        for i in 0..10 {
            bus.publish(i);
        }
        assert!(bus.dropped() >= 8, "dropped {}", bus.dropped());
        drop(gate_tx);
        bus.close();
    }
}
