//! Voice tick lateness since the server started, as a histogram that yields percentiles.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use tufo_models::playback::diagnostics::VoicePacing;

use super::voice_source::{LATE_TICK, VERY_LATE_TICK};

/// Bucket upper bounds in microseconds: 50 µs steps to 1 ms, 1 ms steps to 20 ms, then 10 ms
/// steps to 100 ms. One more bucket counts everything later.
const BUCKET_BOUNDS_MICROS: [u64; 47] = bucket_bounds();
const BUCKET_COUNT: usize = BUCKET_BOUNDS_MICROS.len() + 1;

const fn bucket_bounds() -> [u64; 47] {
    let mut bounds = [0; 47];
    let mut index = 0;
    while index < 20 {
        bounds[index] = 50 * (index as u64 + 1);
        index += 1;
    }
    while index < 39 {
        bounds[index] = 1_000 * (index as u64 - 18);
        index += 1;
    }
    while index < 47 {
        bounds[index] = 10_000 * (index as u64 - 36);
        index += 1;
    }
    bounds
}

/// Counts every voice tick by lateness. Recording only touches atomics, so songbird's mixer
/// thread never blocks on it.
#[derive(Debug)]
pub(super) struct PacingTotals {
    buckets: [AtomicU64; BUCKET_COUNT],
    late_ticks: AtomicU64,
    very_late_ticks: AtomicU64,
    max_lateness_micros: AtomicU64,
}

impl Default for PacingTotals {
    fn default() -> Self {
        Self {
            buckets: [const { AtomicU64::new(0) }; BUCKET_COUNT],
            late_ticks: AtomicU64::new(0),
            very_late_ticks: AtomicU64::new(0),
            max_lateness_micros: AtomicU64::new(0),
        }
    }
}

impl PacingTotals {
    pub(super) fn record(&self, lateness: Duration) {
        let micros = u64::try_from(lateness.as_micros()).unwrap_or(u64::MAX);
        let bucket = BUCKET_BOUNDS_MICROS.partition_point(|&bound| bound < micros);
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        if lateness > LATE_TICK {
            self.late_ticks.fetch_add(1, Ordering::Relaxed);
        }
        if lateness > VERY_LATE_TICK {
            self.very_late_ticks.fetch_add(1, Ordering::Relaxed);
        }
        self.max_lateness_micros
            .fetch_max(micros, Ordering::Relaxed);
    }

    pub(super) fn summary(&self) -> VoicePacing {
        let counts = self
            .buckets
            .each_ref()
            .map(|bucket| bucket.load(Ordering::Relaxed));
        let ticks = counts.iter().sum::<u64>();
        let max = Duration::from_micros(self.max_lateness_micros.load(Ordering::Relaxed));
        let percentile = |per_mille: u64| percentile(&counts, ticks, per_mille).unwrap_or(max);
        VoicePacing {
            ticks,
            late_ticks: self.late_ticks.load(Ordering::Relaxed),
            very_late_ticks: self.very_late_ticks.load(Ordering::Relaxed),
            lateness_p50: percentile(500).min(max),
            lateness_p99: percentile(990).min(max),
            lateness_p999: percentile(999).min(max),
            lateness_max: max,
        }
    }
}

/// The upper bound of the bucket holding the `per_mille`th tick, or `None` when that tick is past
/// the last bound.
fn percentile(counts: &[u64; BUCKET_COUNT], ticks: u64, per_mille: u64) -> Option<Duration> {
    let rank = (ticks * per_mille).div_ceil(1_000).max(1);
    let mut seen = 0;
    for (bucket, count) in counts.iter().enumerate() {
        seen += count;
        if seen >= rank {
            return BUCKET_BOUNDS_MICROS
                .get(bucket)
                .map(|&bound| Duration::from_micros(bound));
        }
    }
    Some(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_bounds_rise_through_each_resolution() {
        assert_eq!(
            [
                BUCKET_BOUNDS_MICROS[0],
                BUCKET_BOUNDS_MICROS[19],
                BUCKET_BOUNDS_MICROS[20],
                BUCKET_BOUNDS_MICROS[38],
                BUCKET_BOUNDS_MICROS[39],
                BUCKET_BOUNDS_MICROS[46]
            ],
            [50, 1_000, 2_000, 20_000, 30_000, 100_000]
        );
        assert!(
            BUCKET_BOUNDS_MICROS
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        );
    }

    #[test]
    fn percentiles_report_the_bucket_holding_that_rank() {
        let totals = PacingTotals::default();
        for _ in 0..990 {
            totals.record(Duration::from_micros(120));
        }
        for _ in 0..9 {
            totals.record(Duration::from_millis(7));
        }
        totals.record(Duration::from_millis(250));

        let summary = totals.summary();

        assert_eq!(summary.ticks, 1_000);
        assert_eq!(summary.lateness_p50, Duration::from_micros(150));
        assert_eq!(summary.lateness_p99, Duration::from_micros(150));
        assert_eq!(summary.lateness_p999, Duration::from_millis(7));
        assert_eq!(summary.lateness_max, Duration::from_millis(250));
        assert_eq!((summary.late_ticks, summary.very_late_ticks), (10, 1));
    }

    #[test]
    fn no_ticks_report_zero_lateness() {
        let summary = PacingTotals::default().summary();

        assert_eq!(summary.ticks, 0);
        assert_eq!(summary.lateness_p999, Duration::ZERO);
    }
}
