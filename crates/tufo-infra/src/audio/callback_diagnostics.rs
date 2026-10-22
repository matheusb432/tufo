//! Callback observations distinguish rendering cost and cadence from source starvation.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tufo_models::playback::diagnostics::LocalCallbackDiagnostics;

const BUCKET_BOUNDS_MICROS: [u64; 36] = [
    25, 50, 100, 250, 500, 1_000, 2_000, 3_000, 4_000, 5_000, 6_000, 7_000, 8_000, 9_000, 10_000,
    11_000, 12_000, 13_000, 14_000, 15_000, 16_000, 17_000, 18_000, 19_000, 20_000, 25_000, 30_000,
    40_000, 50_000, 75_000, 100_000, 150_000, 250_000, 500_000, 1_000_000, 2_000_000,
];
const BUCKET_COUNT: usize = BUCKET_BOUNDS_MICROS.len() + 1;
const UNKNOWN_LEAD: u64 = u64::MAX;

#[derive(Debug)]
struct DurationHistogram {
    buckets: [AtomicU64; BUCKET_COUNT],
    max_micros: AtomicU64,
}

impl Default for DurationHistogram {
    fn default() -> Self {
        Self {
            buckets: [const { AtomicU64::new(0) }; BUCKET_COUNT],
            max_micros: AtomicU64::new(0),
        }
    }
}

impl DurationHistogram {
    fn record(&self, duration: Duration) {
        let micros = duration_micros(duration);
        let bucket = BUCKET_BOUNDS_MICROS.partition_point(|&bound| bound < micros);
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        self.max_micros.fetch_max(micros, Ordering::Relaxed);
    }

    fn summary(&self) -> (Duration, Duration) {
        let counts = self
            .buckets
            .each_ref()
            .map(|bucket| bucket.load(Ordering::Relaxed));
        let count = counts.iter().sum::<u64>();
        let max = Duration::from_micros(self.max_micros.load(Ordering::Relaxed));
        if count == 0 {
            return (Duration::ZERO, max);
        }
        let rank = (u128::from(count) * 99).div_ceil(100);
        let mut seen = 0u128;
        for (bucket, count) in counts.iter().enumerate() {
            seen += u128::from(*count);
            if seen >= rank {
                let p99 = BUCKET_BOUNDS_MICROS
                    .get(bucket)
                    .copied()
                    .map_or(max, Duration::from_micros);
                return (p99.min(max), max);
            }
        }
        (max, max)
    }
}

/// Only atomics and monotonic clock reads are used while recording a callback.
pub(crate) struct LocalCallbackCounters {
    origin: Instant,
    session_started_at_unix_ms: u64,
    streams_created: AtomicU64,
    latest_stream_id: AtomicU64,
    callbacks: AtomicU64,
    sample_rate: AtomicU32,
    callback_frames_last: AtomicU32,
    callback_frames_min: AtomicU32,
    callback_frames_max: AtomicU32,
    gaps: DurationHistogram,
    renders: DurationHistogram,
    late_callbacks: AtomicU64,
    render_over_budget: AtomicU64,
    first_anomaly_micros: AtomicU64,
    latest_anomaly_micros: AtomicU64,
    playback_lead_micros: AtomicU64,
}

impl Default for LocalCallbackCounters {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            session_started_at_unix_ms: u64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
            )
            .unwrap_or(u64::MAX),
            streams_created: AtomicU64::new(0),
            latest_stream_id: AtomicU64::new(0),
            callbacks: AtomicU64::new(0),
            sample_rate: AtomicU32::new(0),
            callback_frames_last: AtomicU32::new(0),
            callback_frames_min: AtomicU32::new(u32::MAX),
            callback_frames_max: AtomicU32::new(0),
            gaps: DurationHistogram::default(),
            renders: DurationHistogram::default(),
            late_callbacks: AtomicU64::new(0),
            render_over_budget: AtomicU64::new(0),
            first_anomaly_micros: AtomicU64::new(0),
            latest_anomaly_micros: AtomicU64::new(0),
            playback_lead_micros: AtomicU64::new(UNKNOWN_LEAD),
        }
    }
}

impl LocalCallbackCounters {
    pub(super) fn stream(self: &Arc<Self>, sample_rate: u32) -> CallbackObserver {
        CallbackObserver {
            counters: Arc::clone(self),
            stream_id: self.streams_created.fetch_add(1, Ordering::Relaxed) + 1,
            sample_rate,
            previous_callback: None,
        }
    }

    pub(crate) fn snapshot(&self) -> LocalCallbackDiagnostics {
        let (callback_gap_p99, callback_gap_max) = self.gaps.summary();
        let (render_duration_p99, render_duration_max) = self.renders.summary();
        let callbacks = self.callbacks.load(Ordering::Relaxed);
        LocalCallbackDiagnostics {
            session_started_at_unix_ms: self.session_started_at_unix_ms,
            sampled_at: self.origin.elapsed(),
            streams_created: self.streams_created.load(Ordering::Relaxed),
            latest_stream_id: self.latest_stream_id.load(Ordering::Relaxed),
            callbacks,
            sample_rate: self.sample_rate.load(Ordering::Relaxed),
            callback_frames_last: self.callback_frames_last.load(Ordering::Relaxed),
            callback_frames_min: if callbacks == 0 {
                0
            } else {
                self.callback_frames_min.load(Ordering::Relaxed)
            },
            callback_frames_max: self.callback_frames_max.load(Ordering::Relaxed),
            callback_gap_p99,
            callback_gap_max,
            render_duration_p99,
            render_duration_max,
            late_callbacks: self.late_callbacks.load(Ordering::Relaxed),
            render_over_budget: self.render_over_budget.load(Ordering::Relaxed),
            first_anomaly_at: anomaly_duration(self.first_anomaly_micros.load(Ordering::Relaxed)),
            latest_anomaly_at: anomaly_duration(self.latest_anomaly_micros.load(Ordering::Relaxed)),
            playback_lead: match self.playback_lead_micros.load(Ordering::Relaxed) {
                UNKNOWN_LEAD => None,
                micros => Some(Duration::from_micros(micros)),
            },
        }
    }

    fn record_anomaly(&self, at: Instant) {
        let elapsed = duration_micros(at.saturating_duration_since(self.origin)).saturating_add(1);
        let _ = self.first_anomaly_micros.compare_exchange(
            0,
            elapsed,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        self.latest_anomaly_micros
            .fetch_max(elapsed, Ordering::Relaxed);
    }
}

pub(super) struct CallbackObserver {
    counters: Arc<LocalCallbackCounters>,
    stream_id: u64,
    sample_rate: u32,
    previous_callback: Option<(Instant, u32, Duration)>,
}

impl CallbackObserver {
    pub(super) fn record(
        &mut self,
        started: Instant,
        finished: Instant,
        frames: u32,
        playback_lead: Option<Duration>,
    ) {
        if frames == 0 {
            self.previous_callback = None;
            return;
        }
        let counters = &self.counters;
        let period = Duration::from_secs_f64(f64::from(frames) / f64::from(self.sample_rate));
        let mut anomaly = false;
        if let Some((previous, previous_frames, expected)) =
            self.previous_callback.replace((started, frames, period))
        {
            let gap = started.saturating_duration_since(previous);
            counters.gaps.record(gap);
            if previous_frames == frames && gap > expected + expected / 2 {
                counters.late_callbacks.fetch_add(1, Ordering::Relaxed);
                anomaly = true;
            }
        }
        let render_duration = finished.saturating_duration_since(started);
        counters.renders.record(render_duration);
        if render_duration > period {
            counters.render_over_budget.fetch_add(1, Ordering::Relaxed);
            anomaly = true;
        }
        if anomaly {
            counters.record_anomaly(started);
        }
        counters
            .latest_stream_id
            .store(self.stream_id, Ordering::Relaxed);
        counters
            .sample_rate
            .store(self.sample_rate, Ordering::Relaxed);
        counters
            .callback_frames_last
            .store(frames, Ordering::Relaxed);
        counters
            .callback_frames_min
            .fetch_min(frames, Ordering::Relaxed);
        counters
            .callback_frames_max
            .fetch_max(frames, Ordering::Relaxed);
        if let Some(lead) = playback_lead {
            counters.playback_lead_micros.store(
                duration_micros(lead).min(UNKNOWN_LEAD - 1),
                Ordering::Relaxed,
            );
        }
        counters.callbacks.fetch_add(1, Ordering::Relaxed);
    }
}

fn duration_micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn anomaly_duration(encoded: u64) -> Option<Duration> {
    encoded.checked_sub(1).map(Duration::from_micros)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_source_independent_cadence_and_render_anomalies() {
        let counters = Arc::new(LocalCallbackCounters::default());
        let origin = counters.origin;
        let mut observer = counters.stream(48_000);
        for (start_ms, render_ms) in [(0, 1), (10, 1), (40, 12)] {
            let started = origin + Duration::from_millis(start_ms);
            observer.record(
                started,
                started + Duration::from_millis(render_ms),
                480,
                Some(Duration::from_millis(20)),
            );
        }
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.callbacks, 3);
        assert_eq!(snapshot.late_callbacks, 1);
        assert_eq!(snapshot.render_over_budget, 1);
        assert_eq!(snapshot.callback_gap_max, Duration::from_millis(30));
        assert_eq!(snapshot.render_duration_max, Duration::from_millis(12));
        assert_eq!(snapshot.first_anomaly_at, Some(Duration::from_millis(40)));
        assert_eq!(snapshot.latest_anomaly_at, snapshot.first_anomaly_at);
        assert_eq!(snapshot.playback_lead, Some(Duration::from_millis(20)));
    }

    #[test]
    fn replacement_stream_keeps_totals_and_restarts_cadence() {
        let counters = Arc::new(LocalCallbackCounters::default());
        let first = counters.origin;
        counters.stream(48_000).record(first, first, 512, None);
        let later = first + Duration::from_secs(30);
        counters.stream(44_100).record(later, later, 1024, None);
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.streams_created, 2);
        assert_eq!(snapshot.latest_stream_id, 2);
        assert_eq!(snapshot.callbacks, 2);
        assert_eq!(snapshot.sample_rate, 44_100);
        assert_eq!(snapshot.callback_frames_min, 512);
        assert_eq!(snapshot.callback_frames_max, 1024);
        assert_eq!(snapshot.late_callbacks, 0);
        assert_eq!(snapshot.callback_gap_max, Duration::ZERO);
    }

    #[test]
    fn cadence_uses_the_actual_previous_frame_count_and_preserves_thresholds() {
        let counters = Arc::new(LocalCallbackCounters::default());
        let mut observer = counters.stream(48_000);
        let origin = counters.origin;
        for (start_ms, frames) in [(0, 480), (15, 960), (45, 480)] {
            let started = origin + Duration::from_millis(start_ms);
            observer.record(started, started, frames, None);
        }
        assert_eq!(counters.snapshot().late_callbacks, 0);
        let started = origin + Duration::from_millis(61);
        observer.record(started, started + Duration::from_millis(10), 480, None);
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.late_callbacks, 1);
        assert_eq!(snapshot.render_over_budget, 0);
        assert_eq!(snapshot.first_anomaly_at, Some(Duration::from_millis(61)));
    }

    #[test]
    fn quantum_changes_and_empty_callbacks_do_not_report_false_cadence_anomalies() {
        let counters = Arc::new(LocalCallbackCounters::default());
        let mut observer = counters.stream(48_000);
        let origin = counters.origin;
        for (start_ms, frames) in [(0, 512), (43, 2048), (86, 2048), (100, 0), (900, 512)] {
            let started = origin + Duration::from_millis(start_ms);
            observer.record(started, started, frames, None);
        }
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.callbacks, 4);
        assert_eq!(snapshot.late_callbacks, 0);
        assert_eq!(snapshot.render_over_budget, 0);
        assert_eq!(snapshot.callback_gap_max, Duration::from_millis(43));
    }

    #[test]
    fn percentile_retains_rare_long_rendering_and_caps_bucket_rounding() {
        let histogram = DurationHistogram::default();
        for _ in 0..98 {
            histogram.record(Duration::from_micros(30));
        }
        histogram.record(Duration::from_millis(21));
        histogram.record(Duration::from_secs(3));
        assert_eq!(
            histogram.summary(),
            (Duration::from_millis(25), Duration::from_secs(3))
        );
        let empty = LocalCallbackCounters::default().snapshot();
        assert_eq!(empty.callback_frames_min, 0);
        assert_eq!(empty.first_anomaly_at, None);
    }
}
