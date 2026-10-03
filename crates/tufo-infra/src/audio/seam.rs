//! The crossfade where a loop region wraps, and the clip that previews it.

use std::{path::Path, time::Duration};

use kira::{
    Frame,
    sound::static_sound::{StaticSoundData, StaticSoundSettings},
};
use tufo_models::library::song_loop::{LoopRegion, SeamPreviewSide, SourceFrames};

use super::buffered_track::{AudioFile, OpenTrackError, duration_of_frames, frames_in};

const STEREO: usize = 2;
const SEAM_CLIP_EDGE_FADE: Duration = Duration::from_millis(10);
const SEAM_CLIP_SILENCE: Duration = Duration::from_millis(750);

/// A looping clip of the audio either side of a loop's seam, then a short silence.
pub(crate) struct SeamClip {
    pub(crate) sound: StaticSoundData,
    pub(crate) cycle: Duration,
}

/// Each side lasts at most one loop, so the clip never needs a second wrap.
pub(crate) fn render_seam_clip(
    path: &Path,
    region: LoopRegion,
    side: SeamPreviewSide,
) -> Result<SeamClip, OpenTrackError> {
    let timing = region.timing();
    let markers = region.markers();
    let side_frames = timing
        .frames_in(side.into_inner())
        .min(region.loop_length());
    let decode = |start: SourceFrames, frames: SourceFrames| {
        let frames_max = usize::try_from(frames.0).unwrap_or(usize::MAX);
        AudioFile::open(path)?
            .into_probed()
            .decode_range(start, frames_max)
            .map_err(OpenTrackError::Unsupported)
    };
    let mut samples = decode(markers.loop_end.saturating_sub(side_frames), side_frames)?;
    if markers.crossfade > SourceFrames::ZERO {
        let lead_in = decode(
            markers.loop_start.saturating_sub(markers.crossfade),
            markers.crossfade,
        )?;
        let tail_frames = samples.len() / STEREO;
        let blended_frames = (lead_in.len() / STEREO).min(tail_frames);
        fade_in_lead(
            &mut samples[(tail_frames - blended_frames) * STEREO..],
            &lead_in,
            lead_in.len() / STEREO - blended_frames,
        );
    }
    samples.extend(decode(markers.loop_start, side_frames)?);
    let sample_rate = timing.sample_rate.get();
    let mut frames = samples
        .as_chunks::<STEREO>()
        .0
        .iter()
        .map(|&[left, right]| Frame::new(left, right))
        .collect::<Vec<_>>();
    fade_edges(&mut frames, frames_in(SEAM_CLIP_EDGE_FADE, sample_rate));
    frames.resize(
        frames.len() + frames_in(SEAM_CLIP_SILENCE, sample_rate),
        Frame::ZERO,
    );
    let cycle = duration_of_frames(u64::try_from(frames.len()).unwrap_or(u64::MAX), sample_rate);
    Ok(SeamClip {
        sound: StaticSoundData {
            sample_rate,
            frames: frames.into(),
            settings: StaticSoundSettings::default().loop_region(..),
            slice: None,
        },
        cycle,
    })
}

#[expect(
    clippy::cast_precision_loss,
    reason = "edge fades span a few hundred frames"
)]
fn fade_edges(frames: &mut [Frame], fade_frames: usize) {
    let fade_frames = fade_frames.min(frames.len() / 2);
    let length = frames.len();
    for index in 0..fade_frames {
        let gain = index as f32 / fade_frames as f32;
        frames[index] *= gain;
        frames[length - 1 - index] *= gain;
    }
}

/// Linearly fades the interleaved stereo `lead_in`, the frames just before the loop start, in
/// over `tail`, whose first frame lies `tail_offset_frames` into the crossfade.
pub(crate) fn fade_in_lead(tail: &mut [f32], lead_in: &[f32], tail_offset_frames: usize) {
    let lead_in_frames = lead_in.as_chunks::<STEREO>().0;
    #[expect(
        clippy::cast_precision_loss,
        reason = "crossfades span at most ten seconds of frames"
    )]
    let crossfade_frames = lead_in_frames.len() as f32;
    for (index, frame) in tail.as_chunks_mut::<STEREO>().0.iter_mut().enumerate() {
        let offset = tail_offset_frames + index;
        let Some(&[lead_left, lead_right]) = lead_in_frames.get(offset) else {
            return;
        };
        #[expect(
            clippy::cast_precision_loss,
            reason = "crossfades span at most ten seconds of frames"
        )]
        let lead_gain = (offset as f32 + 0.5) / crossfade_frames;
        frame[0] += (lead_left - frame[0]) * lead_gain;
        frame[1] += (lead_right - frame[1]) * lead_gain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tail_hands_over_to_the_lead_in_by_the_last_frame() {
        let mut tail = vec![1.0; 8];

        fade_in_lead(&mut tail, &[0.0; 8], 0);

        let left = tail.iter().step_by(2).copied().collect::<Vec<_>>();
        assert_eq!(left, [0.875, 0.625, 0.375, 0.125]);
    }

    #[test]
    fn a_tail_starting_inside_the_crossfade_continues_its_fade() {
        let mut whole = vec![1.0; 8];
        fade_in_lead(&mut whole, &[0.0; 8], 0);
        let mut second_half = vec![1.0; 4];

        fade_in_lead(&mut second_half, &[0.0; 8], 2);

        assert_eq!(second_half, whole[4..]);
    }
}
