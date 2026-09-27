//! Renders a pulled mixer frame by frame to check what the music deck and the limiter produce.
//!
//! Fixtures last 200 ms, shorter than the decoder's prefill, so starting a track returns only
//! after its whole file is decoded and rendering never outruns the decoder.

use std::{num::NonZeroU64, sync::Arc, time::Duration};

use kira::{
    Frame,
    backend::Renderer,
    sound::static_sound::{StaticSoundData, StaticSoundSettings},
};
use tufo_infra::audio::{
    Mixer,
    buffered_track::{AudioFile, BufferedTrack, DecodeOptions, TrackHandle},
    limiter::LIMITER_CEILING,
    music_deck::{DeckState, DeckTrack},
};

mod support;

use support::{SAMPLE_RATE, assert_near, constant, ramp, render, write_wav};

const TRACK_FRAMES: usize = 9_600;
const FIRST: NonZeroU64 = NonZeroU64::MIN;
const SECOND: NonZeroU64 = NonZeroU64::MIN.saturating_add(1);

struct Fixture {
    mixer: Mixer,
    renderer: Renderer,
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> anyhow::Result<Self> {
        let (mixer, renderer_receiver) = Mixer::open_pulled(SAMPLE_RATE)?;
        Ok(Self {
            mixer,
            renderer: renderer_receiver.recv()?,
            directory: tempfile::tempdir()?,
        })
    }

    /// Starts decoding a fixture holding `samples` and wraps it for the deck as `token`.
    fn track(
        &self,
        token: NonZeroU64,
        samples: &[f32],
        options: DecodeOptions,
    ) -> anyhow::Result<(DeckTrack, TrackHandle)> {
        let path = self.directory.path().join(format!("{token}.wav"));
        write_wav(&path, SAMPLE_RATE, samples)?;
        let BufferedTrack { reader, handle } =
            BufferedTrack::start(AudioFile::open(&path)?, SAMPLE_RATE, options)?;
        Ok((DeckTrack { token, reader }, handle))
    }

    fn deck_track(
        &self,
        token: NonZeroU64,
        samples: &[f32],
        options: DecodeOptions,
    ) -> anyhow::Result<DeckTrack> {
        Ok(self.track(token, samples, options)?.0)
    }

    fn render(&mut self, frames: usize) -> Vec<f32> {
        render(&mut self.renderer, frames)
    }
}

fn all_equal(samples: &[f32], value: f32) -> bool {
    samples
        .iter()
        .all(|sample| sample.to_bits() == value.to_bits())
}

#[test]
fn without_a_crossfade_the_next_track_follows_on_the_very_next_frame() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let first = fixture.deck_track(
        FIRST,
        &constant(0.5, TRACK_FRAMES),
        DecodeOptions::default(),
    )?;
    let second = fixture.deck_track(
        SECOND,
        &constant(-0.25, TRACK_FRAMES),
        DecodeOptions::default(),
    )?;
    let deck = fixture.mixer.music();
    deck.set_crossfade(Duration::ZERO)?;
    deck.start(first, Duration::ZERO, Duration::ZERO)?;
    deck.arm(second)?;

    let output = fixture.render(3 * TRACK_FRAMES);

    assert!(all_equal(&output[..TRACK_FRAMES], 0.5));
    assert!(all_equal(&output[TRACK_FRAMES..2 * TRACK_FRAMES], -0.25));
    assert!(all_equal(&output[2 * TRACK_FRAMES..], 0.0));
    assert_eq!(fixture.mixer.music().state(), DeckState::Idle);
    Ok(())
}

#[test]
fn a_crossfade_blends_the_ending_track_into_the_next_before_it_ends() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let first = fixture.deck_track(
        FIRST,
        &constant(0.5, TRACK_FRAMES),
        DecodeOptions::default(),
    )?;
    let second = fixture.deck_track(
        SECOND,
        &constant(-0.25, TRACK_FRAMES),
        DecodeOptions::default(),
    )?;
    let crossfade_frames = 2_400;
    let deck = fixture.mixer.music();
    deck.set_crossfade(Duration::from_millis(50))?;
    deck.start(first, Duration::ZERO, Duration::ZERO)?;
    deck.arm(second)?;

    let output = fixture.render(3 * TRACK_FRAMES);

    // kira renders in 128-frame periods, so the crossfade starts within one period after its
    // point and lasts until the first track's last frame.
    let blend_start = TRACK_FRAMES - crossfade_frames;
    assert!(all_equal(&output[..blend_start], 0.5));
    let blend = &output[blend_start..TRACK_FRAMES];
    assert!(blend.windows(2).all(|pair| pair[1] <= pair[0]));
    assert!(blend.iter().any(|sample| (0.0..0.4).contains(sample)));
    let second_end = blend_start + TRACK_FRAMES;
    assert!(all_equal(&output[TRACK_FRAMES..second_end], -0.25));
    assert!(all_equal(&output[second_end + 128..], 0.0));
    assert_eq!(fixture.mixer.music().state(), DeckState::Idle);
    Ok(())
}

#[test]
fn the_deck_reports_the_armed_track_once_it_becomes_current() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let first = fixture.deck_track(
        FIRST,
        &constant(0.5, TRACK_FRAMES),
        DecodeOptions::default(),
    )?;
    let second = fixture.deck_track(
        SECOND,
        &constant(-0.25, TRACK_FRAMES),
        DecodeOptions::default(),
    )?;
    let deck = fixture.mixer.music();
    deck.start(first, Duration::ZERO, Duration::ZERO)?;
    deck.arm(second)?;

    let before = fixture.mixer.music().state();
    fixture.render(TRACK_FRAMES / 2);
    let during_first = fixture.mixer.music().state();
    fixture.render(TRACK_FRAMES);
    let during_second = fixture.mixer.music().state();

    assert_eq!(before, DeckState::Pending);
    assert_eq!(during_first, DeckState::Playing(FIRST));
    assert_eq!(during_second, DeckState::Playing(SECOND));
    Ok(())
}

#[test]
fn pausing_holds_the_position_and_resuming_continues_from_it() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let (track, handle) = fixture.track(FIRST, &ramp(TRACK_FRAMES), DecodeOptions::default())?;
    fixture
        .mixer
        .music()
        .start(track, Duration::ZERO, Duration::ZERO)?;

    fixture.render(2_400);
    fixture.mixer.music().pause(Duration::ZERO)?;
    let paused = fixture.render(2_400);
    let played_while_paused = handle.played();
    fixture.mixer.music().resume(Duration::ZERO)?;
    let resumed = fixture.render(480);

    assert!(all_equal(&paused, 0.0));
    assert_eq!(played_while_paused, Duration::from_millis(50));
    assert_near(resumed[0], ramp(2_401)[2_400], 1e-7);
    Ok(())
}

#[test]
fn a_track_can_start_partway_through_its_file() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let track = fixture.deck_track(
        FIRST,
        &ramp(TRACK_FRAMES),
        DecodeOptions {
            start_position: Duration::from_millis(100),
            looping: false,
        },
    )?;
    fixture
        .mixer
        .music()
        .start(track, Duration::ZERO, Duration::ZERO)?;

    let output = fixture.render(TRACK_FRAMES);

    assert_near(output[0], ramp(4_801)[4_800], 1e-7);
    assert!(all_equal(&output[TRACK_FRAMES / 2..], 0.0));
    Ok(())
}

#[test]
fn a_looping_track_wraps_from_its_last_frame_to_its_first() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let frames = 4_800;
    let samples = ramp(frames);
    let track = fixture.deck_track(
        FIRST,
        &samples,
        DecodeOptions {
            start_position: Duration::ZERO,
            looping: true,
        },
    )?;
    fixture
        .mixer
        .music()
        .start(track, Duration::ZERO, Duration::ZERO)?;

    let output = fixture.render(2 * frames + 100);

    assert_eq!(output[..frames], samples[..]);
    assert_eq!(output[frames..2 * frames], samples[..]);
    assert_eq!(output[2 * frames..], samples[..100]);
    Ok(())
}

#[test]
fn the_limiter_keeps_music_plus_sounds_below_full_scale() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let music = fixture.deck_track(
        FIRST,
        &constant(0.8, TRACK_FRAMES),
        DecodeOptions::default(),
    )?;
    fixture
        .mixer
        .music()
        .start(music, Duration::ZERO, Duration::ZERO)?;
    let sound = StaticSoundData {
        sample_rate: SAMPLE_RATE,
        frames: Arc::from(vec![Frame::new(0.8, 0.8); TRACK_FRAMES]),
        settings: StaticSoundSettings::default(),
        slice: None,
    };
    fixture.mixer.play_decoded_sound(sound)?;

    let output = fixture.render(TRACK_FRAMES);

    assert!(output.iter().all(|sample| sample.abs() <= LIMITER_CEILING));
    assert_near(output[TRACK_FRAMES / 2], LIMITER_CEILING, 0.01);
    Ok(())
}
