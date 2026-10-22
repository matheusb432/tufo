//! Aligning an edited preview with the source frame the old draft is about to play.

use std::num::NonZeroU64;

use tufo_models::library::song_loop::{LoopRegion, SourceFrames, nearest_frames};

use crate::audio::buffered_track::duration_of_frames;

#[derive(Clone, Copy)]
pub(crate) struct PreviewPosition {
    pub region: LoopRegion,
    pub start: SourceFrames,
}

impl PreviewPosition {
    pub(crate) fn wraps(self) -> bool {
        self.start < self.region.markers().loop_end
    }

    pub(crate) fn frame_after(self, frames: u64, sample_rate: u32) -> SourceFrames {
        let played = self
            .region
            .timing()
            .frames_in(duration_of_frames(frames, sample_rate));
        self.region.preview_frame_after(self.start, played)
    }

    fn distance_to(self, frame: SourceFrames) -> Option<SourceFrames> {
        let markers = self.region.markers();
        if self.wraps() && frame >= markers.loop_end {
            return None;
        }
        if frame >= self.start {
            return Some(frame.saturating_sub(self.start));
        }
        if !self.wraps() || frame < markers.loop_start {
            return None;
        }
        Some(
            markers
                .loop_end
                .saturating_sub(self.start)
                .saturating_add(frame.saturating_sub(markers.loop_start)),
        )
    }
}

pub(crate) struct PreviewAlignment {
    pub token_previous: NonZeroU64,
    pub previous: PreviewPosition,
    pub replacement: PreviewPosition,
}

impl PreviewAlignment {
    pub(crate) fn skip_frames(&self, played: u64, sample_rate: u32) -> Option<u64> {
        let frame = self.replacement.region.preview_start_continuing(
            self.previous.frame_after(played, sample_rate),
            self.previous.wraps(),
        );
        let distance = self.replacement.distance_to(frame)?;
        Some(nearest_frames(
            self.replacement.region.timing().duration_of(distance),
            sample_rate,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use tufo_models::library::song_loop::{LoopMarkers, SourceTiming};

    use super::*;

    fn position(start: u64, loop_start: u64, loop_end: u64) -> PreviewPosition {
        PreviewPosition {
            region: LoopRegion::try_new(
                SourceTiming {
                    sample_rate: NonZeroU32::new(48_000).unwrap(),
                    frame_count: SourceFrames(480_000),
                },
                LoopMarkers {
                    playback_start: SourceFrames::ZERO,
                    loop_start: SourceFrames(loop_start),
                    loop_end: SourceFrames(loop_end),
                    crossfade: SourceFrames::ZERO,
                },
            )
            .unwrap(),
            start: SourceFrames(start),
        }
    }

    #[test]
    fn replacement_alignment_follows_old_wraps_at_the_output_rate() {
        let alignment = PreviewAlignment {
            token_previous: NonZeroU64::MIN,
            previous: position(81_600, 48_000, 96_000),
            replacement: position(81_600, 48_000, 134_400),
        };
        // After 0.7 s the old loop is at 1.4 s. The new decoder reaches that frame after
        // its first 1.1 s pass and another 0.4 s, not after 0.7 s.
        assert_eq!(alignment.skip_frames(30_870, 44_100), Some(66_150));
    }

    #[test]
    fn shortening_a_loop_continues_at_its_start_when_the_old_frame_is_past_it() {
        let alignment = PreviewAlignment {
            token_previous: NonZeroU64::MIN,
            previous: position(81_600, 48_000, 96_000),
            replacement: position(48_000, 48_000, 57_600),
        };
        assert_eq!(alignment.skip_frames(4_800, 48_000), Some(0));
    }

    #[test]
    fn a_replacement_cannot_reach_an_earlier_intro_or_a_nonlooping_tail() {
        assert_eq!(
            position(81_600, 72_000, 134_400).distance_to(SourceFrames(67_200)),
            None
        );
        assert_eq!(
            position(81_600, 48_000, 96_000).distance_to(SourceFrames(100_800)),
            None
        );
        assert_eq!(
            position(100_800, 48_000, 96_000).distance_to(SourceFrames(96_000)),
            None
        );
        assert_eq!(
            position(100_800, 48_000, 96_000).distance_to(SourceFrames(105_600)),
            Some(SourceFrames(4_800))
        );
    }
}
