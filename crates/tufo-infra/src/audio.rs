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
pub(crate) mod seam;
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
use tufo_models::{library::Volume, playback::music_layers::MUSIC_LAYERS_EXTRA_MAX};

/// Requested local callback size: about 43 ms at 48 kHz. ALSA uses two periods; native
/// `PipeWire` negotiates the graph quantum and promotes its callback through rtkit.
const LOCAL_PERIOD_FRAMES: u32 = 2048;
/// How often kira updates clocks and modulators, in frames; kira's default.
const KIRA_INTERNAL_BUFFER_FRAMES: usize = 128;
const LOCAL_STREAM_ERRORS_POLL_MAX: usize = 64;
/// How long a volume change takes, so moving a slider doesn't click.
const VOLUME_TWEEN: Duration = Duration::from_millis(50);

#[derive(Debug, thiserror::Error)]
pub enum OpenLocalMixerError {
    #[error("no default audio output device")]
    NoDevice,
    #[error("the PipeWire output target {0:?} does not exist")]
    TargetNotFound(String),
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
    manager: MixerManager,
    local_stream_errors: LocalStreamErrors,
    sample_rate: u32,
    music: MusicDeckHandle,
    music_layers: [(MusicDeckHandle, TrackHandle); MUSIC_LAYERS_EXTRA_MAX],
    music_track: TrackHandle,
    sounds_track: TrackHandle,
}

enum MixerManager {
    Local(Box<AudioManager<CpalBackend>>),
    Pulled {
        _manager: Box<AudioManager<PullBackend>>,
    },
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct LocalStreamErrors {
    pub(crate) reported: u64,
    pub(crate) discarded: u64,
}

impl Mixer {
    /// Opens the default audio device at its preferred rate and channel count.
    pub fn open_local(effects_volume: Volume) -> Result<Self, OpenLocalMixerError> {
        #[cfg(target_os = "linux")]
        let pipewire = pipewire_output_device()?;
        #[cfg(target_os = "linux")]
        let device_override = pipewire.as_ref().map(|output| output.device.clone());
        #[cfg(not(target_os = "linux"))]
        let device_override: Option<cpal::Device> = None;
        let device = device_override
            .clone()
            .or_else(|| cpal::default_host().default_output_device())
            .ok_or(OpenLocalMixerError::NoDevice)?;
        let supported = device
            .default_output_config()
            .map_err(OpenLocalMixerError::DeviceConfiguration)?;
        let config = StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: BufferSize::Fixed(LOCAL_PERIOD_FRAMES),
        };
        tracing::info!(device = ?device.id(), sample_rate = config.sample_rate, "opening local audio output");
        let mut manager = AudioManager::<CpalBackend>::new(AudioManagerSettings {
            backend_settings: CpalBackendSettings {
                device: device_override,
                config: Some(config),
            },
            main_track_builder: limited_main_track(),
            ..AudioManagerSettings::default()
        })
        .map_err(OpenLocalMixerError::Stream)?;
        let tracks = MixerTracks::add(&mut manager, config.sample_rate, effects_volume)
            .map_err(OpenLocalMixerError::Tracks)?;
        Ok(tracks.into_mixer(MixerManager::Local(Box::new(manager)), config.sample_rate))
    }

    /// Creates a mixer that renders only when the returned renderer is pulled.
    pub fn open_pulled(
        sample_rate: u32,
        effects_volume: Volume,
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
        let tracks = MixerTracks::add(&mut manager, sample_rate, effects_volume)
            .map_err(OpenPulledMixerError::Tracks)?;
        Ok((
            tracks.into_mixer(
                MixerManager::Pulled {
                    _manager: Box::new(manager),
                },
                sample_rate,
            ),
            renderer_receiver,
        ))
    }

    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub(crate) fn poll_local_stream_errors(&mut self) -> LocalStreamErrors {
        let MixerManager::Local(manager) = &mut self.manager else {
            return LocalStreamErrors::default();
        };
        let backend = manager.backend_mut();
        for _ in 0..LOCAL_STREAM_ERRORS_POLL_MAX {
            let Some(error) = backend.pop_error() else {
                break;
            };
            self.local_stream_errors.reported += 1;
            tracing::warn!(%error, kind = ?error.kind(), "local audio stream error");
        }
        if let Some(discarded) = backend.num_stream_errors_discarded()
            && discarded > self.local_stream_errors.discarded
        {
            tracing::warn!(
                discarded = discarded - self.local_stream_errors.discarded,
                "local audio stream errors were lost because an error queue filled"
            );
            self.local_stream_errors.discarded = discarded;
        }
        self.local_stream_errors
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
            track.set_volume(decibels(volume), linear_tween(VOLUME_TWEEN));
        }
    }

    /// Sets the music's gain, a linear value from silence (0) to full level (1).
    pub fn set_music_volume(&mut self, volume: f32) {
        self.music_track
            .set_volume(decibels(volume), linear_tween(VOLUME_TWEEN));
    }

    /// Multiplies every effect's own gain without changing its settings or playback.
    pub(crate) fn set_effects_volume(&mut self, volume: f32) {
        self.sounds_track
            .set_volume(decibels(volume), linear_tween(VOLUME_TWEEN));
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

#[cfg(target_os = "linux")]
struct PipeWireOutputDevice {
    device: cpal::Device,
    // Keep PipeWire initialized between device enumeration and stream creation.
    _host: cpal::Host,
}

#[cfg(target_os = "linux")]
fn pipewire_output_device() -> Result<Option<PipeWireOutputDevice>, OpenLocalMixerError> {
    let host = match cpal::host_from_id(cpal::HostId::PipeWire) {
        Ok(host) => host,
        Err(error) => {
            tracing::debug!(%error, "PipeWire is unavailable; using the platform audio host");
            return Ok(None);
        }
    };
    let device = if let Ok(target) = std::env::var("PIPEWIRE_NODE") {
        let id = cpal::DeviceId::new(cpal::HostId::PipeWire, &target);
        host.device_by_id(&id)
            .filter(cpal::traits::DeviceTrait::supports_output)
            .ok_or(OpenLocalMixerError::TargetNotFound(target))?
    } else {
        host.default_output_device()
            .ok_or(OpenLocalMixerError::NoDevice)?
    };
    Ok(Some(PipeWireOutputDevice {
        device,
        _host: host,
    }))
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
        effects_volume: Volume,
    ) -> Result<Self, ResourceLimitReached> {
        let mut music_track = manager.add_sub_track(TrackBuilder::new())?;
        // Setting gain after construction interpolates from full volume for one buffer.
        let sounds_track = manager
            .add_sub_track(TrackBuilder::new().volume(decibels(effects_volume.into_inner())))?;
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

    fn into_mixer(self, manager: MixerManager, sample_rate: u32) -> Mixer {
        Mixer {
            manager,
            local_stream_errors: LocalStreamErrors::default(),
            sample_rate,
            music: self.music,
            music_layers: self.music_layers,
            music_track: self.music_track,
            sounds_track: self.sounds_track,
        }
    }
}
