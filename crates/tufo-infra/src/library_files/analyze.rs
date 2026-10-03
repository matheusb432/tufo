//! Decoding a whole file into its exact frame count, waveform peaks, and packet fingerprint.

use std::{io, num::NonZeroU64, path::Path};

use anyhow::Context as _;
use tufo_application::library::library_files::InspectAudioError;
use tufo_models::library::{
    AudioFingerprint,
    song_loop::{SourceFrames, SourceTiming, nearest_frames},
};
use tufo_wire::song_loops::SongWaveform;

use crate::audio::buffered_track::{AudioFile, OpenTrackError};

const PEAK_BUCKETS_MAX: usize = 1 << 18;
const BUCKET_FRAMES_MIN: u64 = 64;
const STEREO: usize = 2;

/// Hashes the same packets as `inspect`, so the fingerprint matches the library's.
pub(super) fn analyze_audio(path: &Path) -> Result<SongWaveform, InspectAudioError> {
    let file = AudioFile::open(path).map_err(|error| match error {
        OpenTrackError::NotFound => {
            InspectAudioError::Unreadable(io::Error::from(io::ErrorKind::NotFound))
        }
        OpenTrackError::Unreadable(source) | OpenTrackError::DecoderThread(source) => {
            InspectAudioError::Unreadable(source)
        }
        OpenTrackError::Unsupported(source) => InspectAudioError::Unsupported(source),
    })?;
    let sample_rate = std::num::NonZeroU32::new(file.source_sample_rate())
        .context("the audio track has no sample rate")
        .map_err(InspectAudioError::Unsupported)?;
    let declared_frames = file
        .duration()
        .map(|duration| nearest_frames(duration, sample_rate.get()));
    let mut peaks = PeakBuckets::new(declared_frames);
    let mut hasher = blake3::Hasher::new();
    let mut probed = file.into_probed();
    let mut decoded = Vec::new();
    let mut stereo = Vec::new();
    let mut frame_count = 0_u64;
    while probed
        .decode_next_packet_observed(&mut decoded, &mut stereo, |packet| {
            hasher.update(packet);
        })
        .map_err(InspectAudioError::Unsupported)?
    {
        for &[left, right] in stereo.as_chunks::<STEREO>().0 {
            peaks.push(left.abs().max(right.abs()));
        }
        frame_count += u64::try_from(stereo.len() / STEREO).unwrap_or(u64::MAX);
    }
    let fingerprint = AudioFingerprint::try_new(hasher.finalize().to_hex().to_string())
        .context("BLAKE3 produced an unexpected digest")
        .map_err(InspectAudioError::Unsupported)?;
    let (bucket_frames, peaks) = peaks.finish();
    Ok(SongWaveform {
        fingerprint,
        timing: SourceTiming {
            sample_rate,
            frame_count: SourceFrames(frame_count),
        },
        bucket_frames,
        peaks,
    })
}

/// Halves its resolution whenever it fills, so files of unknown length stay bounded.
struct PeakBuckets {
    bucket_frames: u64,
    peaks: Vec<u8>,
    open_bucket_peak: f32,
    open_bucket_frames: u64,
}

impl PeakBuckets {
    fn new(declared_frames: Option<u64>) -> Self {
        let bucket_frames = declared_frames
            .map_or(BUCKET_FRAMES_MIN, |frames| {
                frames.div_ceil(PEAK_BUCKETS_MAX as u64).next_power_of_two()
            })
            .max(BUCKET_FRAMES_MIN);
        Self {
            bucket_frames,
            peaks: Vec::new(),
            open_bucket_peak: 0.0,
            open_bucket_frames: 0,
        }
    }

    fn push(&mut self, frame_peak: f32) {
        self.open_bucket_peak = self.open_bucket_peak.max(frame_peak);
        self.open_bucket_frames += 1;
        if self.open_bucket_frames == self.bucket_frames {
            self.close_bucket();
        }
    }

    fn close_bucket(&mut self) {
        self.peaks.push(peak_level(self.open_bucket_peak));
        self.open_bucket_peak = 0.0;
        self.open_bucket_frames = 0;
        if self.peaks.len() == PEAK_BUCKETS_MAX {
            self.peaks = self
                .peaks
                .chunks(2)
                .map(|pair| pair.iter().copied().max().unwrap_or(0))
                .collect();
            self.bucket_frames *= 2;
        }
    }

    fn finish(mut self) -> (NonZeroU64, Vec<u8>) {
        if self.open_bucket_frames > 0 {
            self.close_bucket();
        }
        (
            NonZeroU64::new(self.bucket_frames).unwrap_or(NonZeroU64::MIN),
            self.peaks,
        )
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the clamped level lies within 0..=255"
)]
fn peak_level(peak: f32) -> u8 {
    (peak.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_buckets_merge_in_pairs_and_double_their_length() {
        let mut buckets = PeakBuckets::new(None);
        let frames = BUCKET_FRAMES_MIN * PEAK_BUCKETS_MAX as u64 + 3;

        for frame in 0..frames {
            buckets.push(if frame == 0 { 1.0 } else { 0.5 });
        }
        let (bucket_frames, peaks) = buckets.finish();

        assert_eq!(bucket_frames.get(), BUCKET_FRAMES_MIN * 2);
        assert_eq!(peaks.len(), PEAK_BUCKETS_MAX / 2 + 1);
        assert_eq!(peaks[0], 255);
        assert_eq!(peaks[1], 128);
    }
}
