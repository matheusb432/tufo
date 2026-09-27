//! Effects loop without a seam.

use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use kira::Frame;
use tufo_infra::{
    audio::{
        buffered_track::{AudioFile, BufferedTrack, DecodeOptions, TrackReader},
        sound_cache::STATIC_SOUND_DURATION_MAX,
    },
    default_effects::write_default_effects,
};
use tufo_models::library::default_effects::DefaultEffect;

/// The effects' own rate, so no resampler blurs the seam.
const EFFECT_SAMPLE_RATE: u32 = 44_100;
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const READ_CHUNK_FRAMES: usize = 4_096;

/// Reads `frames` frames, or every frame when the track ends first, waiting for the decoder.
fn read_frames(reader: &mut TrackReader, frames: usize) -> Vec<Frame> {
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut output = Vec::new();
    let mut chunk = vec![Frame::ZERO; READ_CHUNK_FRAMES];
    while output.len() < frames && !reader.is_exhausted() {
        assert!(Instant::now() < deadline, "the decoder stalled");
        let wanted = (frames - output.len()).min(READ_CHUNK_FRAMES);
        let copied = reader.read(&mut chunk[..wanted]);
        output.extend_from_slice(&chunk[..copied]);
        if copied == 0 {
            thread::sleep(Duration::from_millis(1));
        }
    }
    output
}

/// The first frame where the passes differ, including where one ends early.
fn first_mismatch(pass: &[Frame], expected: &[Frame]) -> Option<usize> {
    (0..pass.len().max(expected.len())).find(|&index| pass.get(index) != expected.get(index))
}

fn decode(path: &std::path::Path, looping: bool) -> anyhow::Result<TrackReader> {
    let options = DecodeOptions {
        start_position: Duration::ZERO,
        looping,
    };
    let BufferedTrack { reader, handle: _ } =
        BufferedTrack::start(AudioFile::open(path)?, EFFECT_SAMPLE_RATE, options)?;
    Ok(reader)
}

/// Fireplace is the shortest effect that streams rather than decoding into memory.
#[test]
fn a_streamed_effect_plays_its_second_pass_frame_for_frame_like_its_first() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let fireplace = write_default_effects(directory.path())?
        .into_iter()
        .find(|file| file.effect == DefaultEffect::Fireplace)
        .context("Fireplace is a default effect")?;

    let first_pass = read_frames(&mut decode(fireplace.path.as_ref(), false)?, usize::MAX);
    let looped = read_frames(
        &mut decode(fireplace.path.as_ref(), true)?,
        2 * first_pass.len(),
    );

    let decoded_frames_max = STATIC_SOUND_DURATION_MAX.as_secs() * u64::from(EFFECT_SAMPLE_RATE);
    assert!(u64::try_from(first_pass.len())? > decoded_frames_max);
    let (first_loop, second_loop) = looped.split_at(first_pass.len());
    assert_eq!(first_mismatch(first_loop, &first_pass), None);
    assert_eq!(first_mismatch(second_loop, &first_pass), None);
    Ok(())
}
