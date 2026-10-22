//! Measurements that show whether playback kept its timing since the server started.

use std::time::Duration;

/// Playback health totals since the server started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayerDiagnostics {
    /// Frames of silence music playback inserted because a decoder fell behind.
    pub music_underrun_frames: u64,
    /// Frames of silence streamed effects inserted because a decoder fell behind.
    pub sound_underrun_frames: u64,
    /// Local output stream errors reported by CPAL, across output changes.
    pub local_stream_errors: u64,
    /// Local stream errors lost when Tufo's bounded error queue filled.
    pub local_stream_errors_discarded: u64,
    /// Frames of silence browser audio inserted because a page's packets arrived late.
    pub browser_underrun_frames: u64,
    /// Browser frames discarded because they arrived faster than the output played them, or
    /// while the output changed.
    pub browser_dropped_frames: u64,
    /// How late songbird ran its 20 ms voice ticks, absent without Discord.
    pub voice_pacing: Option<VoicePacing>,
    /// Callback observations across local output changes; these are not backend xrun counts.
    pub local_callbacks: Option<LocalCallbackDiagnostics>,
    pub audio_memory: Option<AudioMemoryDiagnostics>,
}

/// Process-wide prepared audio memory and best-effort residency protection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioMemoryDiagnostics {
    pub bytes_used: u64,
    pub bytes_max: u64,
    pub locked_bytes: u64,
    pub lock_failures: u64,
    pub lock_failed_bytes: u64,
    pub locking_supported: bool,
}

/// Bounded measurements on the local rendering thread, retained across stream replacement.
/// Percentiles are histogram bucket upper bounds, capped at the observed maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LocalCallbackDiagnostics {
    pub session_started_at_unix_ms: u64,
    pub sampled_at: Duration,
    /// Includes a stream creation whose native open subsequently fails.
    pub streams_created: u64,
    /// The most recent stream to finish a callback, numbered from one.
    pub latest_stream_id: u64,
    /// Completed callbacks containing at least one audio frame.
    pub callbacks: u64,
    /// Rate of the most recent callback; frame bounds span all streams.
    pub sample_rate: u32,
    pub callback_frames_last: u32,
    pub callback_frames_min: u32,
    pub callback_frames_max: u32,
    pub callback_gap_p99: Duration,
    pub callback_gap_max: Duration,
    pub render_duration_p99: Duration,
    pub render_duration_max: Duration,
    /// Gaps greater than 1.5 frame periods, excluding quantum changes and empty callbacks.
    pub late_callbacks: u64,
    /// Rendering took longer than the current callback's frame duration.
    pub render_over_budget: u64,
    /// Time since this diagnostics session began, absent until an anomaly is observed.
    pub first_anomaly_at: Option<Duration>,
    pub latest_anomaly_at: Option<Duration>,
    /// Last nonnegative playback-minus-callback timestamp reported by CPAL.
    pub playback_lead: Option<Duration>,
}

/// How late songbird's mixer thread ran its 20 ms ticks, against an ideal grid.
///
/// Percentiles are upper bounds of histogram buckets: exact below 1 ms to 50 µs, then to 1 ms up
/// to 20 ms, then to 10 ms up to 100 ms. Lateness beyond 100 ms reports as `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VoicePacing {
    pub ticks: u64,
    /// Ticks more than 5 ms late.
    pub late_ticks: u64,
    /// Ticks more than 20 ms late, which songbird sends as bursts.
    pub very_late_ticks: u64,
    pub lateness_p50: Duration,
    pub lateness_p99: Duration,
    pub lateness_p999: Duration,
    pub lateness_max: Duration,
}
