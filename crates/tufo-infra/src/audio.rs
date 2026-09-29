//! The audio client: decodes files ahead of playback and mixes them with kira on one output.
//!
//! Every mixer has the same shape. A music track holds the [`music_deck`], a sounds track holds
//! the sound effects, and the main track limits their sum so it never clips.

pub mod buffered_track;
pub(crate) mod codecs;
mod envelope;
pub mod limiter;
pub mod music_deck;
pub mod pull_backend;
pub mod sound_cache;
pub mod streamed_sound;

use std::{
    sync::mpsc::{Receiver, sync_channel},
    time::Duration,
};

use cpal::{
    BufferSize, StreamConfig,
    traits::{DeviceTrait as _, HostTrait as _},
};
use kira::{
    AudioManager, AudioManagerSettings, Decibels, ResourceLimitReached, StartTime, Tween,
    backend::{
        Backend, Renderer,
        cpal::{CpalBackend, CpalBackendSettings},
    },
    sound::static_sound::{StaticSoundData, StaticSoundHandle},
    track::{MainTrackBuilder, TrackBuilder, TrackHandle},
};
use limiter::LimiterBuilder;
use music_deck::MusicDeckHandle;
use pull_backend::{PullBackend, PullBackendError, PullBackendSettings};
use streamed_sound::StreamedSound;
use tufo_models::playback::music_layers::MUSIC_LAYERS_EXTRA_MAX;

/// ALSA period size for the local device. Two periods of 2048 frames, about 85 ms at 48 kHz,
/// absorb scheduling delays of cpal's callback thread, which runs at normal priority when ALSA
/// routes into `PipeWire`.
const LOCAL_PERIOD_FRAMES: u32 = 2048;
/// How often kira updates clocks and modulators, in frames; kira's default.
const KIRA_INTERNAL_BUFFER_FRAMES: usize = 128;
/// How long a music volume change takes, so moving a slider doesn't click.
const MUSIC_VOLUME_TWEEN: Duration = Duration::from_millis(50);

#[derive(Debug, thiserror::Error)]
pub enum OpenLocalMixerError {
    #[error("no default audio output device")]
    NoDevice,
    #[error("the default audio device reports no usable configuration")]
    DeviceConfiguration(#[source] cpal::Error),
    #[error("the audio output stream cannot start")]
    Stream(#[source] kira::backend::cpal::Error),
    #[error("the mixer cannot hold its tracks")]
    Tracks(#[source] ResourceLimitReached),
}

#[derive(Debug, thiserror::Error)]
pub enum OpenPulledMixerError {
    #[error(transparent)]
    Backend(#[from] PullBackendError),
    #[error("the mixer cannot hold its tracks")]
    Tracks(#[source] ResourceLimitReached),
}

#[derive(Debug, thiserror::Error)]
#[error("the mixer is already playing its maximum number of sounds")]
pub struct MixerFullError;

/// A kira mixer bound to one output, with the sample rate its tracks must be decoded at.
pub struct Mixer {
    /// The kira manager, held only so its output stream stops when the mixer drops.
    _manager: Box<dyn Send>,
    sample_rate: u32,
    music: MusicDeckHandle,
    music_layers: [(MusicDeckHandle, TrackHandle); MUSIC_LAYERS_EXTRA_MAX],
    music_track: TrackHandle,
    sounds_track: TrackHandle,
}

impl Mixer {
    /// Opens the default audio device at its preferred rate and channel count.
    pub fn open_local() -> Result<Self, OpenLocalMixerError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or(OpenLocalMixerError::NoDevice)?;
        let supported = device
            .default_output_config()
            .map_err(OpenLocalMixerError::DeviceConfiguration)?;
        let config = StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: BufferSize::Fixed(LOCAL_PERIOD_FRAMES),
        };
        let mut manager = AudioManager::<CpalBackend>::new(AudioManagerSettings {
            backend_settings: CpalBackendSettings {
                device: None,
                config: Some(config),
            },
            main_track_builder: limited_main_track(),
            ..AudioManagerSettings::default()
        })
        .map_err(OpenLocalMixerError::Stream)?;
        let tracks = MixerTracks::add(&mut manager, config.sample_rate)
            .map_err(OpenLocalMixerError::Tracks)?;
        Ok(tracks.into_mixer(Box::new(manager), config.sample_rate))
    }

    /// Creates a mixer that renders only when the returned renderer is pulled.
    pub fn open_pulled(
        sample_rate: u32,
    ) -> Result<(Self, Receiver<Renderer>), OpenPulledMixerError> {
        let (renderer_sender, renderer_receiver) = sync_channel(1);
        let mut manager = AudioManager::<PullBackend>::new(AudioManagerSettings {
            backend_settings: PullBackendSettings {
                sample_rate,
                renderer_sender,
            },
            capacities: kira::Capacities::default(),
            main_track_builder: limited_main_track(),
            internal_buffer_size: KIRA_INTERNAL_BUFFER_FRAMES,
        })?;
        let tracks =
            MixerTracks::add(&mut manager, sample_rate).map_err(OpenPulledMixerError::Tracks)?;
        Ok((
            tracks.into_mixer(Box::new(manager), sample_rate),
            renderer_receiver,
        ))
    }

    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The deck that plays music on this mixer.
    pub fn music(&mut self) -> &mut MusicDeckHandle {
        &mut self.music
    }

    pub(crate) fn music_layer(&mut self, slot: usize) -> Option<&mut MusicDeckHandle> {
        self.music_layers.get_mut(slot).map(|(deck, _)| deck)
    }

    pub(crate) fn set_music_layer_volume(&mut self, slot: usize, volume: f32) {
        if let Some((_, track)) = self.music_layers.get_mut(slot) {
            track.set_volume(decibels(volume), linear_tween(MUSIC_VOLUME_TWEEN));
        }
    }

    /// Sets the music's gain, a linear value from silence (0) to full level (1).
    pub fn set_music_volume(&mut self, volume: f32) {
        self.music_track
            .set_volume(decibels(volume), linear_tween(MUSIC_VOLUME_TWEEN));
    }

    /// Fades every sound out over `fade` and holds it where it is.
    pub fn pause_sounds(&mut self, fade: Duration) {
        self.sounds_track.pause(linear_tween(fade));
    }

    pub fn resume_sounds(&mut self, fade: Duration) {
        self.sounds_track.resume(linear_tween(fade));
    }

    pub fn play_decoded_sound(
        &mut self,
        sound: StaticSoundData,
    ) -> Result<StaticSoundHandle, MixerFullError> {
        self.sounds_track.play(sound).map_err(|_| MixerFullError)
    }

    pub(crate) fn play_streamed_sound(
        &mut self,
        sound: StreamedSound,
    ) -> Result<(), MixerFullError> {
        self.sounds_track.play(sound).map_err(|_| MixerFullError)
    }
}

/// Converts a linear gain to kira's decibels, where −60 dB is silence.
#[must_use]
pub fn decibels(volume: f32) -> Decibels {
    if volume <= 0.0 {
        return Decibels::SILENCE;
    }
    Decibels((20.0 * volume.log10()).max(Decibels::SILENCE.0))
}

fn linear_tween(duration: Duration) -> Tween {
    Tween {
        start_time: StartTime::Immediate,
        duration,
        easing: kira::Easing::Linear,
    }
}

fn limited_main_track() -> MainTrackBuilder {
    MainTrackBuilder::new().with_effect(LimiterBuilder)
}

struct MixerTracks {
    music: MusicDeckHandle,
    music_layers: [(MusicDeckHandle, TrackHandle); MUSIC_LAYERS_EXTRA_MAX],
    music_track: TrackHandle,
    sounds_track: TrackHandle,
}

impl MixerTracks {
    fn add<B: Backend>(
        manager: &mut AudioManager<B>,
        sample_rate: u32,
    ) -> Result<Self, ResourceLimitReached> {
        let mut music_track = manager.add_sub_track(TrackBuilder::new())?;
        let sounds_track = manager.add_sub_track(TrackBuilder::new())?;
        let (deck, music) = music_deck::music_deck(sample_rate);
        music_track.play(deck).map_err(|_| ResourceLimitReached)?;
        let mut add_layer = || {
            let (deck, handle) = music_deck::music_deck(sample_rate);
            let mut track = music_track.add_sub_track(TrackBuilder::new())?;
            track.play(deck).map_err(|_| ResourceLimitReached)?;
            Ok::<_, ResourceLimitReached>((handle, track))
        };
        let music_layers = [add_layer()?, add_layer()?];
        Ok(Self {
            music,
            music_layers,
            music_track,
            sounds_track,
        })
    }

    fn into_mixer(self, manager: Box<dyn Send>, sample_rate: u32) -> Mixer {
        Mixer {
            _manager: manager,
            sample_rate,
            music: self.music,
            music_layers: self.music_layers,
            music_track: self.music_track,
            sounds_track: self.sounds_track,
        }
    }
}
