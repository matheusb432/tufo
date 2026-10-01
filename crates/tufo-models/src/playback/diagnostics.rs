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
    /// Local stream errors lost when CPAL or Kira's error queues filled.
    pub local_stream_errors_discarded: u64,
    /// How late songbird ran its 20 ms voice ticks, absent without Discord.
    pub voice_pacing: Option<VoicePacing>,
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
