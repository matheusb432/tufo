//! The PCM stream songbird's mixer thread pulls once per 20 ms tick.
//!
//! Each read renders at most one tick of the kira mixer, so rendering follows songbird's packet
//! clock instead of running ahead, and the moment of each render measures how late songbird's
//! tick ran. The thread that keeps pulling ticks is songbird's mixer thread, so the source promotes
//! it to real-time priority; the pool thread that probes the stream reads only its first bytes.

use std::{
    cell::Cell,
    io::{self, Read, Seek, SeekFrom},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::Receiver,
    },
    time::{Duration, Instant},
};

use kira::backend::Renderer;
use songbird::input::core::io::MediaSource;

use super::{DISCORD_SAMPLE_RATE, pacing_totals::PacingTotals};

const TICK_FRAMES: u32 = 960;
/// Ticks a thread renders before the source treats it as songbird's mixer thread.
const PROMOTE_AFTER_TICKS: u32 = 10;
const STEREO: u16 = 2;
/// One tick of interleaved stereo samples.
const TICK_SAMPLES: usize = 960 * 2;
const TICK_DURATION: Duration = Duration::from_millis(20);
/// A pause longer than this means songbird stopped pulling, so the schedule restarts.
const RESYNC_GAP: Duration = Duration::from_secs(1);
pub(super) const LATE_TICK: Duration = Duration::from_millis(5);
pub(super) const VERY_LATE_TICK: Duration = Duration::from_millis(20);

pub(super) struct KiraVoiceSource {
    // `MediaSource` requires `Sync`; reads take `&mut self`, so the lock is never contended.
    state: Mutex<VoiceSourceState>,
}

struct VoiceSourceState {
    renderer_receiver: Receiver<Renderer>,
    renderer: Option<Renderer>,
    samples: Vec<f32>,
    bytes: Vec<u8>,
    bytes_read: usize,
    schedule: TickSchedule,
    stats: Arc<VoicePacingStats>,
    totals: Arc<PacingTotals>,
}

impl KiraVoiceSource {
    /// Records each tick's lateness into the session's `stats` and the server's `totals`.
    pub(super) fn new(
        renderer_receiver: Receiver<Renderer>,
        stats: Arc<VoicePacingStats>,
        totals: Arc<PacingTotals>,
    ) -> Self {
        Self {
            state: Mutex::new(VoiceSourceState {
                renderer_receiver,
                renderer: None,
                samples: vec![0.0; TICK_SAMPLES],
                bytes: Vec::with_capacity(TICK_SAMPLES * size_of::<f32>()),
                bytes_read: 0,
                schedule: TickSchedule::default(),
                stats,
                totals,
            }),
        }
    }
}

impl VoiceSourceState {
    fn render_tick(&mut self, now: Instant) {
        if self.renderer.is_none() {
            self.renderer = self.renderer_receiver.try_recv().ok();
        }
        match &mut self.renderer {
            Some(renderer) => {
                renderer.on_start_processing();
                renderer.process(&mut self.samples, STEREO);
            }
            None => self.samples.fill(0.0),
        }
        self.bytes.clear();
        self.bytes
            .extend(self.samples.iter().flat_map(|sample| sample.to_le_bytes()));
        self.bytes_read = 0;
        let lateness = self.schedule.lateness(now);
        self.stats.record(lateness);
        self.totals.record(lateness);
        promote_steady_puller(&self.stats);
    }
}

thread_local! {
    static TICKS_RENDERED_ON_THIS_THREAD: Cell<u32> = const { Cell::new(0) };
}

/// Promotes the current thread once it has rendered [`PROMOTE_AFTER_TICKS`] ticks.
fn promote_steady_puller(stats: &VoicePacingStats) {
    let ticks = TICKS_RENDERED_ON_THIS_THREAD.get().saturating_add(1);
    TICKS_RENDERED_ON_THIS_THREAD.set(ticks);
    if ticks == PROMOTE_AFTER_TICKS {
        let outcome = audio_thread_priority::promote_current_thread_to_real_time(
            TICK_FRAMES,
            DISCORD_SAMPLE_RATE,
        )
        .map(|_handle| ())
        .map_err(|error| error.to_string());
        stats.record_realtime_promotion(outcome);
    }
}

impl Read for KiraVoiceSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let state = self
            .state
            .get_mut()
            .map_err(|_| io::Error::other("the voice source state is poisoned"))?;
        if state.bytes_read == state.bytes.len() {
            state.render_tick(Instant::now());
        }
        let unread = &state.bytes[state.bytes_read..];
        let count = unread.len().min(buf.len());
        buf[..count].copy_from_slice(&unread[..count]);
        state.bytes_read += count;
        Ok(count)
    }
}

impl Seek for KiraVoiceSource {
    fn seek(&mut self, _position: SeekFrom) -> io::Result<u64> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

impl MediaSource for KiraVoiceSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// Measures each tick against an ideal 20 ms grid anchored at the earliest tick seen.
#[derive(Debug, Default)]
struct TickSchedule {
    anchor: Option<ScheduleAnchor>,
}

#[derive(Debug, Clone, Copy)]
struct ScheduleAnchor {
    origin: Instant,
    ticks: u32,
    last: Instant,
}

impl TickSchedule {
    /// Returns how far behind the grid `now` is. An early tick moves the grid instead.
    fn lateness(&mut self, now: Instant) -> Duration {
        let anchor = match self.anchor {
            Some(anchor) if now.saturating_duration_since(anchor.last) <= RESYNC_GAP => anchor,
            _ => {
                self.anchor = Some(ScheduleAnchor {
                    origin: now,
                    ticks: 1,
                    last: now,
                });
                return Duration::ZERO;
            }
        };
        let ideal_elapsed = TICK_DURATION * anchor.ticks;
        let (origin, lateness) = match now.checked_duration_since(anchor.origin + ideal_elapsed) {
            Some(lateness) => (anchor.origin, lateness),
            None => (
                now.checked_sub(ideal_elapsed).unwrap_or(now),
                Duration::ZERO,
            ),
        };
        self.anchor = Some(ScheduleAnchor {
            origin,
            ticks: anchor.ticks.saturating_add(1),
            last: now,
        });
        lateness
    }
}

/// Pacing counters shared between songbird's mixer thread and the reporter task. The mixer
/// thread only touches atomics, so it never blocks on logging.
#[derive(Debug, Default)]
pub(super) struct VoicePacingStats {
    ticks: AtomicU64,
    late_ticks: AtomicU64,
    very_late_ticks: AtomicU64,
    max_lateness_micros: AtomicU64,
    realtime_promotion: OnceLock<Result<(), String>>,
}

/// Pacing over one reporting window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PacingWindow {
    pub(super) ticks: u64,
    /// Ticks more than 5 ms late.
    pub(super) late_ticks: u64,
    /// Ticks more than 20 ms late: songbird sends these as bursts.
    pub(super) very_late_ticks: u64,
    pub(super) max_lateness: Duration,
}

impl VoicePacingStats {
    fn record(&self, lateness: Duration) {
        self.ticks.fetch_add(1, Ordering::Relaxed);
        if lateness > LATE_TICK {
            self.late_ticks.fetch_add(1, Ordering::Relaxed);
        }
        if lateness > VERY_LATE_TICK {
            self.very_late_ticks.fetch_add(1, Ordering::Relaxed);
        }
        let micros = u64::try_from(lateness.as_micros()).unwrap_or(u64::MAX);
        self.max_lateness_micros
            .fetch_max(micros, Ordering::Relaxed);
    }

    pub(super) fn take_window(&self) -> PacingWindow {
        PacingWindow {
            ticks: self.ticks.swap(0, Ordering::Relaxed),
            late_ticks: self.late_ticks.swap(0, Ordering::Relaxed),
            very_late_ticks: self.very_late_ticks.swap(0, Ordering::Relaxed),
            max_lateness: Duration::from_micros(
                self.max_lateness_micros.swap(0, Ordering::Relaxed),
            ),
        }
    }

    fn record_realtime_promotion(&self, outcome: Result<(), String>) {
        let _ = self.realtime_promotion.set(outcome);
    }

    pub(super) fn realtime_promotion(&self) -> Option<&Result<(), String>> {
        self.realtime_promotion.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lateness_at(offsets_ms: &[u64]) -> Vec<u64> {
        let start = Instant::now();
        let mut schedule = TickSchedule::default();
        offsets_ms
            .iter()
            .map(|&offset| {
                let lateness = schedule.lateness(start + Duration::from_millis(offset));
                u64::try_from(lateness.as_millis()).unwrap()
            })
            .collect()
    }

    #[test]
    fn on_time_ticks_are_never_late() {
        assert_eq!(lateness_at(&[0, 20, 40, 60]), [0, 0, 0, 0]);
    }

    #[test]
    fn a_stall_is_late_until_the_burst_catches_up() {
        assert_eq!(
            lateness_at(&[0, 20, 90, 90, 90, 100]),
            [0, 0, 50, 30, 10, 0]
        );
    }

    #[test]
    fn an_early_tick_moves_the_grid() {
        assert_eq!(lateness_at(&[0, 20, 30, 50, 75]), [0, 0, 0, 0, 5]);
    }

    #[test]
    fn a_long_pause_restarts_the_grid() {
        assert_eq!(lateness_at(&[0, 20, 3_000, 3_020]), [0, 0, 0, 0]);
    }

    #[test]
    fn windows_count_late_ticks_and_reset() {
        let stats = VoicePacingStats::default();
        for lateness_ms in [0, 6, 25] {
            stats.record(Duration::from_millis(lateness_ms));
        }

        let window = stats.take_window();

        assert_eq!(
            window,
            PacingWindow {
                ticks: 3,
                late_ticks: 2,
                very_late_ticks: 1,
                max_lateness: Duration::from_millis(25),
            }
        );
        assert_eq!(stats.take_window().ticks, 0);
    }
}
