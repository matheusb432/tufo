//! WAV and Ogg Opus fixtures, and renderer pulling for tests that listen to a mixer.

#![allow(
    dead_code,
    reason = "each test binary compiles this module but uses a subset"
)]

use std::{io::Write as _, path::Path};

use kira::{Frame, backend::Renderer};
use tufo_infra::audio::packed_frame::PackedFrame;

mod ogg_opus;

#[allow(
    unused_imports,
    reason = "each test binary compiles this module but uses a subset"
)]
pub use ogg_opus::write_ogg_opus;

pub const SAMPLE_RATE: u32 = 48_000;

/// Writes a mono 32-bit float WAV file holding exactly `samples`.
pub fn write_wav(path: &Path, sample_rate: u32, samples: &[f32]) -> anyhow::Result<()> {
    let data_bytes = u32::try_from(samples.len() * 4)?;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    // IEEE float, one channel.
    wav.extend_from_slice(&3_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 4).to_le_bytes());
    wav.extend_from_slice(&4_u16.to_le_bytes());
    wav.extend_from_slice(&32_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::File::create(path)?.write_all(&wav)?;
    Ok(())
}

pub fn constant(value: f32, frames: usize) -> Vec<f32> {
    vec![value; frames]
}

/// A ramp rising by 0.1 per second at [`SAMPLE_RATE`], so a sample's value names its position.
#[expect(
    clippy::cast_precision_loss,
    reason = "fixture frame counts stay far below 2^24"
)]
pub fn ramp(frames: usize) -> Vec<f32> {
    (0..frames).map(|frame| frame as f32 / 480_000.0).collect()
}

/// `samples` as buffered tracks and decoded sounds store them.
pub fn stored(samples: &[f32]) -> Vec<f32> {
    samples
        .iter()
        .map(|&sample| Frame::from(PackedFrame::new(sample, sample)).left)
        .collect()
}

/// The position a ramp sample names, in seconds.
pub fn ramp_position_seconds(sample: f32) -> f32 {
    sample * 10.0
}

/// Renders `frames` stereo frames and returns the left channel.
pub fn render(renderer: &mut Renderer, frames: usize) -> Vec<f32> {
    let mut samples = vec![0.0; frames * 2];
    renderer.on_start_processing();
    renderer.process(&mut samples, 2);
    samples
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[left, _]| left)
        .collect()
}

pub fn assert_near(actual: f32, expected: f32, tolerance: f32) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} is not within {tolerance} of {expected}"
    );
}

/// The sample rate of [`wav_bytes`] tones, which only need to be decodable.
const TONE_SAMPLE_RATE: u32 = 8_000;

/// A mono 16-bit PCM WAV of `seconds` of a tone at `frequency`, optionally followed by a LIST
/// chunk holding a title tag.
pub fn wav_bytes(seconds: u32, frequency: f64, title_tag: Option<&str>) -> Vec<u8> {
    let samples = (0..TONE_SAMPLE_RATE * seconds)
        .map(|index| {
            let phase =
                std::f64::consts::TAU * frequency * f64::from(index) / f64::from(TONE_SAMPLE_RATE);
            #[expect(clippy::cast_possible_truncation, reason = "the tone stays within i16")]
            let sample = (phase.sin() * 8_000.0) as i16;
            sample
        })
        .flat_map(i16::to_le_bytes)
        .collect::<Vec<_>>();
    let mut chunks = Vec::new();
    chunks.extend_from_slice(b"fmt ");
    chunks.extend_from_slice(&16_u32.to_le_bytes());
    chunks.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    chunks.extend_from_slice(&1_u16.to_le_bytes()); // mono
    chunks.extend_from_slice(&TONE_SAMPLE_RATE.to_le_bytes());
    chunks.extend_from_slice(&(TONE_SAMPLE_RATE * 2).to_le_bytes());
    chunks.extend_from_slice(&2_u16.to_le_bytes());
    chunks.extend_from_slice(&16_u16.to_le_bytes());
    chunks.extend_from_slice(b"data");
    chunks.extend_from_slice(&chunk_length(samples.len()));
    chunks.extend_from_slice(&samples);
    if let Some(title) = title_tag {
        let mut value = title.as_bytes().to_vec();
        value.push(0);
        if value.len() % 2 == 1 {
            value.push(0);
        }
        let mut info = b"INFO".to_vec();
        info.extend_from_slice(b"INAM");
        info.extend_from_slice(&chunk_length(value.len()));
        info.extend_from_slice(&value);
        chunks.extend_from_slice(b"LIST");
        chunks.extend_from_slice(&chunk_length(info.len()));
        chunks.extend_from_slice(&info);
    }
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&chunk_length(chunks.len() + 4));
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(&chunks);
    bytes
}

fn chunk_length(length: usize) -> [u8; 4] {
    u32::try_from(length).unwrap_or(u32::MAX).to_le_bytes()
}

/// Interleaved stereo samples of a sine at `frequency`, the same in both channels.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "fixture frame counts stay far below 2^24, and the sine stays within f32"
)]
pub fn stereo_sine(frames: usize, frequency: f64, amplitude: f32) -> Vec<f32> {
    (0..frames)
        .map(|frame| {
            let phase = std::f64::consts::TAU * frequency * frame as f64 / f64::from(SAMPLE_RATE);
            amplitude * phase.sin() as f32
        })
        .flat_map(|sample| [sample, sample])
        .collect()
}

/// How closely two signals match in shape, from -1 (inverted) to 1 (the same).
pub fn correlation(left: &[f32], right: &[f32]) -> f32 {
    let (product, left_energy, right_energy) = left.iter().zip(right).fold(
        (0.0_f64, 0.0_f64, 0.0_f64),
        |(product, left_energy, right_energy), (&left, &right)| {
            let (left, right) = (f64::from(left), f64::from(right));
            (
                product + left * right,
                left_energy + left * left,
                right_energy + right * right,
            )
        },
    );
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the ratio lies within [-1, 1]"
    )]
    let correlation = (product / (left_energy * right_energy).sqrt()) as f32;
    correlation
}
