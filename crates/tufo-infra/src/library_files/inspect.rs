//! Fingerprinting an audio file by its compressed packets, and timing it exactly. A file counts as
//! audio only when a decoder exists for its codec.
//!
//! Demuxing reads packets without decoding them, which costs little more than reading the file.
//! Tags live outside the packets, so retagging a file leaves its fingerprint alone.

use std::{fs::File, io, path::Path, time::Duration};

use anyhow::Context as _;
use symphonia::core::{
    codecs::CodecParameters,
    errors,
    formats::{FormatOptions, TrackType, probe::Hint},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
    units::{self, TimeBase},
};
use tufo_application::library::library_files::{AudioFacts, InspectAudioError};
use tufo_models::library::AudioFingerprint;

use crate::audio::codecs;

/// Enough packets for about 30 hours of 20 ms Opus; a file with more is treated as broken.
const PACKETS_MAX: usize = 5_000_000;

pub(super) fn inspect_audio(path: &Path) -> Result<AudioFacts, InspectAudioError> {
    let file = File::open(path).map_err(InspectAudioError::Unreadable)?;
    let source = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut reader = symphonia::default::get_probe()
        .probe(
            &Hint::default(),
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .context("probing the container")
        .map_err(InspectAudioError::Unsupported)?;
    let track = reader
        .default_track(TrackType::Audio)
        .context("the file has no audio track")
        .map_err(InspectAudioError::Unsupported)?;
    let Some(CodecParameters::Audio(parameters)) = track.codec_params.as_ref() else {
        return Err(InspectAudioError::Unsupported(anyhow::anyhow!(
            "the default track is not audio"
        )));
    };
    if codecs::registry()
        .get_audio_decoder(parameters.codec)
        .is_none()
    {
        return Err(InspectAudioError::Unsupported(anyhow::anyhow!(
            "no decoder reads its {} codec",
            parameters.codec
        )));
    }
    let track_id = track.id;
    let decoder_delay = codecs::decoder_delay(parameters, track);
    let time_base = track
        .time_base
        .or_else(|| {
            parameters
                .sample_rate
                .and_then(|rate| TimeBase::try_new(1, rate))
        })
        .context("the audio track has neither a time base nor a sample rate")
        .map_err(InspectAudioError::Unsupported)?;

    let mut hasher = blake3::Hasher::new();
    let mut total = units::Duration::ZERO;
    for _ in 0..PACKETS_MAX {
        let packet = match reader.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => return facts(&hasher, time_base, total, decoder_delay),
            Err(errors::Error::IoError(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return facts(&hasher, time_base, total, decoder_delay);
            }
            Err(errors::Error::IoError(error)) => return Err(InspectAudioError::Unreadable(error)),
            Err(error) => {
                return Err(InspectAudioError::Unsupported(
                    anyhow::Error::new(error).context("reading a packet"),
                ));
            }
        };
        if packet.track_id == track_id {
            hasher.update(&packet.data);
            total = total
                .checked_add(packet.dur)
                .unwrap_or(units::Duration::MAX);
        }
    }
    Err(InspectAudioError::Unsupported(anyhow::anyhow!(
        "the file holds more than {PACKETS_MAX} packets"
    )))
}

fn facts(
    hasher: &blake3::Hasher,
    time_base: TimeBase,
    total: units::Duration,
    decoder_delay: Duration,
) -> Result<AudioFacts, InspectAudioError> {
    let fingerprint = AudioFingerprint::try_new(hasher.finalize().to_hex().to_string())
        .context("BLAKE3 produced an unexpected digest")
        .map_err(InspectAudioError::Unsupported)?;
    let milliseconds = time_base
        .calc_duration(total)
        .and_then(|time| Duration::try_from_secs_f64(time.as_secs_f64()).ok())
        .map_or(0, |played| {
            u64::try_from(played.saturating_sub(decoder_delay).as_millis()).unwrap_or(0)
        });
    Ok(AudioFacts {
        fingerprint,
        duration: Duration::from_millis(milliseconds),
    })
}
