use std::{
    num::NonZeroU32,
    thread,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use kira::Frame;
use tufo_infra::audio::{
    buffered_track::{AudioFile, BufferedTrack, DecodeOptions, TrackReader},
    packed_frame::PACKED_SAMPLE_STEP,
};
use tufo_models::library::song_loop::{LoopMarkers, LoopRegion, SourceFrames, SourceTiming};

mod support;

use support::{
    SAMPLE_RATE, constant, correlation, ramp, stereo_sine, stored, write_ogg_opus, write_wav,
};

const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Waits until the decoder has buffered `frames` ahead of the reader.
fn wait_for_buffered(reader: &TrackReader, frames: usize) {
    let deadline = Instant::now() + READ_TIMEOUT;
    while reader.buffered_frames() < frames {
        assert!(Instant::now() < deadline, "the decoder stalled");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_started_stream_fills_thirty_seconds_ahead_of_playback() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("ambience.wav");
    write_wav(
        &path,
        SAMPLE_RATE,
        &constant(0.25, 40 * SAMPLE_RATE as usize),
    )?;
    let mut track = BufferedTrack::start(
        AudioFile::open(&path)?,
        SAMPLE_RATE,
        DecodeOptions::default(),
    )?;
    let mut frames = vec![Frame::ZERO; 30 * SAMPLE_RATE as usize];

    wait_for_buffered(&track.reader, frames.len());
    let copied = track.reader.read(&mut frames);

    assert_eq!(copied, frames.len());
    assert!(frames.iter().all(|frame| *frame == Frame::new(0.25, 0.25)));
    assert_eq!(track.handle.played(), Duration::from_secs(30));
    assert_eq!(track.handle.underrun_frames(), 0);
    Ok(())
}

#[test]
fn enabling_a_loop_after_prefill_restarts_a_decoder_that_reached_eof() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("ambience.wav");
    let pass_frames = SAMPLE_RATE as usize * 21;
    write_wav(&path, 8_000, &constant(0.25, 8_000 * 21))?;
    let mut track = BufferedTrack::start(
        AudioFile::open(&path)?,
        SAMPLE_RATE,
        DecodeOptions::default(),
    )?;
    wait_for_buffered(&track.reader, pass_frames);
    track.handle.set_looping(true);
    let mut copied = 0;
    let mut out = [Frame::ZERO; 1_024];
    let deadline = Instant::now() + Duration::from_secs(2);
    while copied < 2 * pass_frames {
        assert!(
            Instant::now() < deadline,
            "the loop did not resume after EOF"
        );
        let wanted = (2 * pass_frames - copied).min(out.len());
        let count = track.reader.read(&mut out[..wanted]);
        assert!(
            out[..count]
                .iter()
                .all(|frame| frame.left.is_finite() && frame.right.is_finite())
        );
        copied += count;
        if count == 0 {
            thread::sleep(Duration::from_millis(1));
        }
    }
    assert!(!track.reader.is_exhausted());
    Ok(())
}

#[test]
fn disabling_a_loop_discards_prefetched_repeats_after_the_current_pass() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("ambience.wav");
    write_wav(&path, SAMPLE_RATE, &constant(0.25, SAMPLE_RATE as usize))?;
    let mut track = BufferedTrack::start(
        AudioFile::open(&path)?,
        SAMPLE_RATE,
        DecodeOptions {
            looping: true,
            ..DecodeOptions::default()
        },
    )?;
    let mut half_pass = vec![Frame::ZERO; SAMPLE_RATE as usize / 2];
    wait_for_buffered(&track.reader, 2 * half_pass.len());
    assert_eq!(track.reader.read(&mut half_pass), half_pass.len());
    track.handle.set_looping(false);

    assert_eq!(track.reader.read(&mut half_pass), half_pass.len());
    assert_eq!(track.reader.read(&mut half_pass), 0);
    assert!(track.reader.is_exhausted());
    assert_eq!(track.handle.played(), Duration::from_secs(1));
    assert_eq!(track.handle.underrun_frames(), 0);
    Ok(())
}

fn loop_region(frame_count: usize, markers: [u64; 4]) -> anyhow::Result<LoopRegion> {
    let [playback_start, loop_start, loop_end, crossfade] = markers.map(SourceFrames);
    Ok(LoopRegion::try_new(
        SourceTiming {
            sample_rate: NonZeroU32::new(SAMPLE_RATE).context("the sample rate is zero")?,
            frame_count: SourceFrames(u64::try_from(frame_count)?),
        },
        LoopMarkers {
            playback_start,
            loop_start,
            loop_end,
            crossfade,
        },
    )?)
}

fn start_region(
    path: &std::path::Path,
    output_sample_rate: u32,
    region: LoopRegion,
    looping: bool,
) -> anyhow::Result<BufferedTrack> {
    Ok(BufferedTrack::start(
        AudioFile::open(path)?,
        output_sample_rate,
        DecodeOptions {
            start_position: region.playback_start(),
            looping,
            loop_region: Some(region),
        },
    )?)
}

fn first_mismatch(played: &[f32], expected: &[f32]) -> Option<(usize, f32, f32)> {
    played
        .iter()
        .zip(expected)
        .enumerate()
        .find(|(_, (played, expected))| played.to_bits() != expected.to_bits())
        .map(|(index, (played, expected))| (index, *played, *expected))
}

fn read_left(reader: &mut TrackReader, frames: usize) -> Vec<f32> {
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut left = Vec::new();
    let mut chunk = vec![Frame::ZERO; 4_096];
    while left.len() < frames && !reader.is_exhausted() {
        assert!(Instant::now() < deadline, "the decoder stalled");
        let wanted = (frames - left.len()).min(chunk.len());
        let copied = reader.read(&mut chunk[..wanted]);
        left.extend(chunk[..copied].iter().map(|frame| frame.left));
        if copied == 0 {
            thread::sleep(Duration::from_millis(1));
        }
    }
    left
}

#[test]
fn a_region_plays_its_intro_once_then_repeats_its_loop_frame_for_frame() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("battle.wav");
    let samples = ramp(SAMPLE_RATE as usize * 3);
    write_wav(&path, SAMPLE_RATE, &samples)?;
    let region = loop_region(samples.len(), [1_000, 48_000, 96_000, 0])?;
    let BufferedTrack { mut reader, .. } = start_region(&path, SAMPLE_RATE, region, true)?;
    let passes = 40;

    let played = read_left(&mut reader, 95_000 + passes * 48_000);

    let samples = stored(&samples);
    let mut expected = samples[1_000..96_000].to_vec();
    for _ in 0..passes {
        expected.extend_from_slice(&samples[48_000..96_000]);
    }
    assert_eq!(played.len(), expected.len());
    assert_eq!(first_mismatch(&played, &expected), None);
    Ok(())
}

#[test]
fn a_region_that_does_not_loop_ends_at_its_loop_end() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("battle.wav");
    let samples = ramp(SAMPLE_RATE as usize * 2);
    write_wav(&path, SAMPLE_RATE, &samples)?;
    let region = loop_region(samples.len(), [4_800, 24_000, 72_000, 0])?;
    let BufferedTrack { mut reader, .. } = start_region(&path, SAMPLE_RATE, region, false)?;

    let played = read_left(&mut reader, samples.len());

    assert_eq!(played, stored(&samples[4_800..72_000]));
    assert!(reader.is_exhausted());
    Ok(())
}

#[test]
fn the_audio_before_the_loop_start_fades_in_over_the_loop_end() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("battle.wav");
    let samples = ramp(SAMPLE_RATE as usize * 2);
    write_wav(&path, SAMPLE_RATE, &samples)?;
    let crossfade = 480;
    let region = loop_region(samples.len(), [0, 24_000, 72_000, crossfade])?;
    let BufferedTrack { mut reader, .. } = start_region(&path, SAMPLE_RATE, region, true)?;

    let played = read_left(&mut reader, 72_000 + 48_000 + 100);

    let crossfade = usize::try_from(crossfade)?;
    for offset in 0..crossfade {
        let tail = samples[72_000 - crossfade + offset];
        let lead_in = samples[24_000 - crossfade + offset];
        #[expect(clippy::cast_precision_loss, reason = "offsets stay below 2^24")]
        let lead_gain = (offset as f32 + 0.5) / crossfade as f32;
        let expected = tail + (lead_in - tail) * lead_gain;
        assert!(
            (played[72_000 - crossfade + offset] - expected).abs() <= PACKED_SAMPLE_STEP / 2.0,
            "frame {offset} of the crossfade"
        );
    }
    assert_eq!(played[72_000..72_100], stored(&samples[24_000..24_100]));
    assert_eq!(
        played[72_000 + 48_000 - crossfade - 10..72_000 + 48_000 - crossfade],
        stored(&samples[72_000 - crossfade - 10..72_000 - crossfade])
    );
    Ok(())
}

#[test]
fn disabling_a_region_loop_ends_the_pass_playing_after_resampling() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("battle.wav");
    let samples = constant(0.25, SAMPLE_RATE as usize * 2);
    write_wav(&path, SAMPLE_RATE, &samples)?;
    let region = loop_region(samples.len(), [1_234, 30_001, 77_777, 0])?;
    let output_sample_rate = 44_100;
    let BufferedTrack { mut reader, handle } =
        start_region(&path, output_sample_rate, region, true)?;
    let source_to_output = |frames: u64| frames * u64::from(output_sample_rate) / 48_000;
    let third_pass_end = source_to_output((77_777 - 1_234) + 2 * (77_777 - 30_001));

    let first_reads = read_left(&mut reader, usize::try_from(third_pass_end)? - 1_000);
    handle.set_looping(false);
    let rest = read_left(&mut reader, usize::MAX);

    assert_eq!(
        u64::try_from(first_reads.len() + rest.len())?,
        third_pass_end
    );
    assert!(reader.is_exhausted());
    Ok(())
}

#[test]
fn opus_seeks_land_on_the_frame_continuous_decoding_reaches() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("battle.opus");
    let frames = SAMPLE_RATE as usize * 4;
    write_ogg_opus(&path, 2, &stereo_sine(frames, 441.0, 0.5))?;
    let target = 100_037;
    let continuous = read_left(
        &mut BufferedTrack::start(
            AudioFile::open(&path)?,
            SAMPLE_RATE,
            DecodeOptions::default(),
        )?
        .reader,
        target + 4_800,
    );

    let seeked = read_left(
        &mut BufferedTrack::start(
            AudioFile::open(&path)?,
            SAMPLE_RATE,
            DecodeOptions {
                start_position: Duration::from_nanos(
                    u64::try_from(target)? * 1_000_000_000 / u64::from(SAMPLE_RATE),
                ),
                ..DecodeOptions::default()
            },
        )?
        .reader,
        4_800,
    );

    let largest_difference = seeked
        .iter()
        .zip(&continuous[target..])
        .map(|(seeked, continuous)| (seeked - continuous).abs())
        .fold(0.0_f32, f32::max);
    assert!(largest_difference < 1e-4, "differs by {largest_difference}");
    Ok(())
}

#[test]
fn an_opus_region_repeats_identical_passes_after_its_first() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("battle.opus");
    let frames = SAMPLE_RATE as usize * 4;
    write_ogg_opus(&path, 2, &stereo_sine(frames, 330.0, 0.5))?;
    let region = loop_region(frames, [0, 48_123, 144_321, 0])?;
    let BufferedTrack { mut reader, .. } = start_region(&path, SAMPLE_RATE, region, true)?;
    let loop_frames = 144_321 - 48_123;

    let played = read_left(&mut reader, 144_321 + 3 * loop_frames);

    let second_pass = &played[144_321..144_321 + loop_frames];
    let third_pass = &played[144_321 + loop_frames..144_321 + 2 * loop_frames];
    assert_eq!(second_pass, third_pass);
    assert!(correlation(&played[48_123..144_321], second_pass) > 0.999);
    Ok(())
}
