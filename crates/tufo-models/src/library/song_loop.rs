//! A song's saved playback start and loop region.

use std::{num::NonZeroU32, time::Duration};

use jiff::Timestamp;
use nutype::nutype;

use super::{AudioFileId, AudioFingerprint};

pub const LOOP_CROSSFADE_MAX: Duration = Duration::from_secs(10);
pub const SEAM_PREVIEW_SIDE_MAX: Duration = Duration::from_secs(10);

/// How much audio a seam preview plays on each side of the seam.
#[nutype(
    validate(predicate = |side| !side.is_zero() && *side <= SEAM_PREVIEW_SIDE_MAX),
    derive(Debug, Clone, Copy, PartialEq, Eq)
)]
pub struct SeamPreviewSide(Duration);

/// A position or length in frames at a file's own sample rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SourceFrames(pub u64);

impl SourceFrames {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    #[must_use]
    pub fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

/// A file's own sample rate and the exact number of frames its decoder produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceTiming {
    pub sample_rate: NonZeroU32,
    pub frame_count: SourceFrames,
}

impl SourceTiming {
    #[must_use]
    pub fn duration(self) -> Duration {
        self.duration_of(self.frame_count)
    }

    #[must_use]
    pub fn duration_of(self, frames: SourceFrames) -> Duration {
        let nanoseconds = u128::from(frames.0) * 1_000_000_000 / u128::from(self.sample_rate.get());
        Duration::from_nanos(u64::try_from(nanoseconds).unwrap_or(u64::MAX))
    }

    /// Rounds to the nearest frame, so it inverts [`Self::duration_of`].
    #[must_use]
    pub fn frames_in(self, duration: Duration) -> SourceFrames {
        SourceFrames(nearest_frames(duration, self.sample_rate.get()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LoopMarkers {
    pub playback_start: SourceFrames,
    pub loop_start: SourceFrames,
    pub loop_end: SourceFrames,
    /// The audio just before the loop start fades in over this many frames before the loop end,
    /// so each pass after the first lasts exactly `loop_end - loop_start`.
    pub crossfade: SourceFrames,
}

impl LoopMarkers {
    /// Starts at the first frame and loops the whole file without a crossfade.
    #[must_use]
    pub fn whole_file(frame_count: SourceFrames) -> Self {
        Self {
            loop_end: frame_count,
            ..Self::default()
        }
    }
}

/// Holds `playback_start <= loop_start < loop_end <= frame_count` and a crossfade within
/// [`crossfade_max`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LoopRegion {
    timing: SourceTiming,
    markers: LoopMarkers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LoopRegionError {
    #[error("the playback start comes after the loop start")]
    PlaybackStartAfterLoopStart,
    #[error("the loop ends before it starts")]
    EmptyLoop,
    #[error("the loop ends after the file's last frame")]
    LoopEndPastFileEnd,
    #[error("the crossfade is longer than {crossfade_max:?} frames")]
    CrossfadeTooLong { crossfade_max: SourceFrames },
}

impl LoopRegion {
    pub fn try_new(timing: SourceTiming, markers: LoopMarkers) -> Result<Self, LoopRegionError> {
        if markers.playback_start > markers.loop_start {
            return Err(LoopRegionError::PlaybackStartAfterLoopStart);
        }
        if markers.loop_start >= markers.loop_end {
            return Err(LoopRegionError::EmptyLoop);
        }
        if markers.loop_end > timing.frame_count {
            return Err(LoopRegionError::LoopEndPastFileEnd);
        }
        let crossfade_max = crossfade_max(timing, markers.loop_start, markers.loop_end);
        if markers.crossfade > crossfade_max {
            return Err(LoopRegionError::CrossfadeTooLong { crossfade_max });
        }
        Ok(Self { timing, markers })
    }

    #[must_use]
    pub fn timing(&self) -> SourceTiming {
        self.timing
    }

    #[must_use]
    pub fn markers(&self) -> LoopMarkers {
        self.markers
    }

    #[must_use]
    pub fn playback_start(&self) -> Duration {
        self.timing.duration_of(self.markers.playback_start)
    }

    #[must_use]
    pub fn loop_end(&self) -> Duration {
        self.timing.duration_of(self.markers.loop_end)
    }

    #[must_use]
    pub fn loop_length(&self) -> SourceFrames {
        self.markers
            .loop_end
            .saturating_sub(self.markers.loop_start)
    }

    /// Moves earlier positions to the playback start; `None` from the loop end on.
    #[must_use]
    pub fn clamp_start(&self, start: Duration) -> Option<Duration> {
        (start < self.loop_end()).then(|| start.max(self.playback_start()))
    }

    /// Pass 0 is the one `start` falls in.
    #[must_use]
    pub fn frames_to_pass_end(&self, start: SourceFrames, pass: u64) -> SourceFrames {
        let whole_loops = SourceFrames(pass.saturating_mul(self.loop_length().0));
        self.markers
            .loop_end
            .saturating_sub(start)
            .saturating_add(whole_loops)
    }

    #[must_use]
    pub fn source_frame_after(&self, start: SourceFrames, played: SourceFrames) -> SourceFrames {
        let first_pass = self.markers.loop_end.saturating_sub(start);
        if played < first_pass {
            return start.saturating_add(played);
        }
        let into_loop = (played.0 - first_pass.0) % self.loop_length().0;
        SourceFrames(self.markers.loop_start.0 + into_loop)
    }

    #[must_use]
    pub fn position_after(&self, start: Duration, played: Duration) -> Duration {
        let frame =
            self.source_frame_after(self.timing.frames_in(start), self.timing.frames_in(played));
        self.timing.duration_of(frame)
    }
}

#[must_use]
pub fn nearest_frames(duration: Duration, sample_rate: u32) -> u64 {
    let rate = u128::from(sample_rate);
    let frames = (duration.as_nanos() * rate + 500_000_000) / 1_000_000_000;
    u64::try_from(frames).unwrap_or(u64::MAX)
}

/// The shortest of [`LOOP_CROSSFADE_MAX`], the loop, and the audio before the loop start.
#[must_use]
pub fn crossfade_max(
    timing: SourceTiming,
    loop_start: SourceFrames,
    loop_end: SourceFrames,
) -> SourceFrames {
    timing
        .frames_in(LOOP_CROSSFADE_MAX)
        .min(loop_start)
        .min(loop_end.saturating_sub(loop_start))
}

/// The database enforces [`LoopRegion`]'s invariants.
#[derive(Debug, toasty::Model)]
#[table = "song_loops"]
pub struct SongLoop {
    #[key]
    pub audio_file_id: AudioFileId,
    /// The file's fingerprint when saved; a file whose fingerprint differs needs review.
    pub fingerprint: AudioFingerprint,
    pub sample_rate: u32,
    pub frame_count: u64,
    pub playback_start_frame: u64,
    pub loop_start_frame: u64,
    pub loop_end_frame: u64,
    pub crossfade_frames: u64,
    pub updated_at: Timestamp,
}

impl SongLoop {
    #[must_use]
    pub fn markers(&self) -> LoopMarkers {
        LoopMarkers {
            playback_start: SourceFrames(self.playback_start_frame),
            loop_start: SourceFrames(self.loop_start_frame),
            loop_end: SourceFrames(self.loop_end_frame),
            crossfade: SourceFrames(self.crossfade_frames),
        }
    }

    pub fn region(&self) -> Result<LoopRegion, SongLoopRowError> {
        let sample_rate =
            NonZeroU32::new(self.sample_rate).ok_or(SongLoopRowError::ZeroSampleRate)?;
        let timing = SourceTiming {
            sample_rate,
            frame_count: SourceFrames(self.frame_count),
        };
        LoopRegion::try_new(timing, self.markers()).map_err(SongLoopRowError::Region)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SongLoopRowError {
    #[error("the stored sample rate is zero")]
    ZeroSampleRate,
    #[error("the stored region is invalid")]
    Region(#[source] LoopRegionError),
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: NonZeroU32 = NonZeroU32::new(48_000).unwrap();

    fn timing(frame_count: u64) -> SourceTiming {
        SourceTiming {
            sample_rate: RATE,
            frame_count: SourceFrames(frame_count),
        }
    }

    fn markers(playback_start: u64, loop_start: u64, loop_end: u64, crossfade: u64) -> LoopMarkers {
        LoopMarkers {
            playback_start: SourceFrames(playback_start),
            loop_start: SourceFrames(loop_start),
            loop_end: SourceFrames(loop_end),
            crossfade: SourceFrames(crossfade),
        }
    }

    fn region(playback_start: u64, loop_start: u64, loop_end: u64) -> LoopRegion {
        LoopRegion::try_new(
            timing(1_000),
            markers(playback_start, loop_start, loop_end, 0),
        )
        .unwrap()
    }

    #[test]
    fn markers_must_be_ordered_and_inside_the_file() {
        let try_region = |markers| LoopRegion::try_new(timing(100), markers);

        assert_eq!(
            try_region(markers(20, 10, 50, 0)),
            Err(LoopRegionError::PlaybackStartAfterLoopStart)
        );
        assert_eq!(
            try_region(markers(0, 50, 50, 0)),
            Err(LoopRegionError::EmptyLoop)
        );
        assert_eq!(
            try_region(markers(0, 10, 101, 0)),
            Err(LoopRegionError::LoopEndPastFileEnd)
        );
        assert!(try_region(markers(10, 10, 100, 0)).is_ok());
    }

    #[test]
    fn the_crossfade_fits_the_loop_and_the_audio_before_it() {
        let try_region = |loop_start, loop_end, crossfade| {
            LoopRegion::try_new(
                timing(10_000_000),
                markers(0, loop_start, loop_end, crossfade),
            )
        };
        let too_long = |frames| {
            Err(LoopRegionError::CrossfadeTooLong {
                crossfade_max: SourceFrames(frames),
            })
        };

        assert!(try_region(100, 1_000, 100).is_ok());
        assert_eq!(try_region(100, 1_000, 101), too_long(100));
        assert_eq!(try_region(1_000, 1_050, 51), too_long(50));
        assert_eq!(try_region(1_000_000, 2_000_000, 480_001), too_long(480_000));
    }

    #[test]
    fn positions_wrap_from_the_loop_end_to_the_loop_start() {
        let region = region(10, 100, 400);
        let after =
            |start, played| region.source_frame_after(SourceFrames(start), SourceFrames(played));

        assert_eq!(after(10, 0), SourceFrames(10));
        assert_eq!(after(10, 389), SourceFrames(399));
        assert_eq!(after(10, 390), SourceFrames(100));
        assert_eq!(after(10, 390 + 299), SourceFrames(399));
        assert_eq!(after(10, 390 + 300 * 1_000), SourceFrames(100));
        assert_eq!(after(250, 150), SourceFrames(100));
    }

    #[test]
    fn pass_ends_count_whole_loops_after_the_first_pass() {
        let region = region(10, 100, 400);

        assert_eq!(
            region.frames_to_pass_end(SourceFrames(10), 0),
            SourceFrames(390)
        );
        assert_eq!(
            region.frames_to_pass_end(SourceFrames(10), 2),
            SourceFrames(990)
        );
        assert_eq!(
            region.frames_to_pass_end(SourceFrames(300), 1),
            SourceFrames(400)
        );
    }

    #[test]
    fn starts_move_into_the_playable_span() {
        let region = region(48, 96, 480);

        assert_eq!(
            region.clamp_start(Duration::ZERO),
            Some(Duration::from_millis(1))
        );
        assert_eq!(
            region.clamp_start(Duration::from_millis(5)),
            Some(Duration::from_millis(5))
        );
        assert_eq!(region.clamp_start(Duration::from_millis(10)), None);
    }

    #[test]
    fn frames_and_durations_convert_exactly_at_whole_milliseconds() {
        let timing = timing(96_000);

        assert_eq!(timing.duration(), Duration::from_secs(2));
        assert_eq!(
            timing.frames_in(Duration::from_millis(1_500)),
            SourceFrames(72_000)
        );
        assert_eq!(
            timing.duration_of(SourceFrames(72_000)),
            Duration::from_millis(1_500)
        );
    }
}
