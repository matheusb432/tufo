use std::{
    thread,
    time::{Duration, Instant},
};

use kira::Frame;
use tufo_infra::audio::buffered_track::{AudioFile, BufferedTrack, DecodeOptions};

mod support;

use support::{SAMPLE_RATE, constant, write_wav};

#[test]
fn a_started_stream_holds_thirty_seconds_without_needing_the_decoder() -> anyhow::Result<()> {
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
    assert_eq!(track.reader.read(&mut half_pass), half_pass.len());
    track.handle.set_looping(false);

    assert_eq!(track.reader.read(&mut half_pass), half_pass.len());
    assert_eq!(track.reader.read(&mut half_pass), 0);
    assert!(track.reader.is_exhausted());
    assert_eq!(track.handle.played(), Duration::from_secs(1));
    assert_eq!(track.handle.underrun_frames(), 0);
    Ok(())
}
