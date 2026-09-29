use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

static REQUESTS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static COMPLETIONS: AtomicU64 = AtomicU64::new(0);
static PIXELS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
pub(crate) struct Counters {
    pub(crate) requests: u64,
    pub(crate) misses: u64,
    pub(crate) completions: u64,
    pub(crate) pixels: u64,
}

impl Counters {
    pub(crate) fn since(self, before: Self) -> Self {
        Self {
            requests: self.requests - before.requests,
            misses: self.misses - before.misses,
            completions: self.completions - before.completions,
            pixels: self.pixels - before.pixels,
        }
    }
}

pub(super) fn requested() {
    REQUESTS.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn missed() {
    MISSES.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn completed() {
    COMPLETIONS.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn drawn(width: u32, height: u32) {
    PIXELS.fetch_add(u64::from(width) * u64::from(height), Ordering::Relaxed);
}

pub(crate) fn snapshot() -> Counters {
    Counters {
        requests: REQUESTS.load(Ordering::Relaxed),
        misses: MISSES.load(Ordering::Relaxed),
        completions: COMPLETIONS.load(Ordering::Relaxed),
        pixels: PIXELS.load(Ordering::Relaxed),
    }
}
