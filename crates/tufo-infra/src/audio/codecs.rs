//! The decoders playback and import share: symphonia's, plus Opus, which symphonia only demuxes.

use std::{sync::LazyLock, time::Duration};

use symphonia::core::{
    codecs::{
        audio::{AudioCodecId, AudioCodecParameters, well_known::CODEC_ID_OPUS},
        registry::CodecRegistry,
    },
    formats::Track,
};

mod opus;

static REGISTRY: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    registry.register_audio_decoder::<opus::OpusDecoder>();
    registry
});

pub(crate) fn registry() -> &'static CodecRegistry {
    &REGISTRY
}

/// How long before a seek target decoding must start for the output to be right at the target.
pub(crate) fn seek_preroll(codec: AudioCodecId) -> Duration {
    if codec == CODEC_ID_OPUS {
        opus::SEEK_PREROLL
    } else {
        Duration::ZERO
    }
}

/// The start of the track that its timestamps count but the decoder drops: Opus's pre-skip, which
/// symphonia's Ogg reader leaves in.
pub(crate) fn decoder_delay(parameters: &AudioCodecParameters, track: &Track) -> Duration {
    match (parameters.codec, track.delay, parameters.sample_rate) {
        (CODEC_ID_OPUS, Some(frames), Some(rate)) if rate > 0 => {
            Duration::from_secs_f64(f64::from(frames) / f64::from(rate))
        }
        _ => Duration::ZERO,
    }
}
