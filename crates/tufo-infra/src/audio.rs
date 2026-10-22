//! The audio client: decodes files ahead of playback and mixes them with kira on one output.
//!
//! Every mixer has the same shape. A music track holds the [`music_deck`], a sounds track holds
//! the sound effects, a browser track holds the pages' [`browser_input`], and the main track
//! limits their sum so it never clips.

pub mod browser_input;
pub mod buffered_track;
pub(crate) mod callback_diagnostics;
pub(crate) mod codecs;
pub mod effect_sound;
mod envelope;
pub mod limiter;
mod local_backend;
pub mod memory;
pub mod music_deck;
pub mod packed_frame;
pub mod pull_backend;
pub(crate) mod seam;
pub mod sound_cache;

use std::{
    sync::{
        Arc,
        mpsc::{Receiver, sync_channel},
    },
    time::{Duration, Instant},
};

use browser_input::{BrowserAudioCounters, BrowserInputHandle};
use callback_diagnostics::LocalCallbackCounters;
use cpal::{
    BufferSize, StreamConfig,
    traits::{DeviceTrait as _, HostTrait as _},
};
use effect_sound::EffectSound;
use kira::{
    AudioManager, AudioManagerSettings, Decibels, ResourceLimitReached, StartTime, Tween,
    backend::{Backend, Renderer},
    sound::static_sound::{StaticSoundData, StaticSoundHandle},
    track::{MainTrackBuilder, TrackBuilder, TrackHandle},
};
use limiter::LimiterBuilder;
use local_backend::{LocalBackend, LocalBackendSettings};
use music_deck::MusicDeckHandle;
use pull_backend::{PullBackend, PullBackendError, PullBackendSettings};
use tufo_models::{
    library::Volume,
    playback::{
        local_output::{LocalOutputFailure, LocalOutputFailureReason, LocalOutputIdentity},
        music_layers::MUSIC_LAYERS_EXTRA_MAX,
    },
};

/// Requested local callback size: about 43 ms at 48 kHz. ALSA uses two periods; native
/// `PipeWire` negotiates the graph quantum and promotes its callback through rtkit.
const LOCAL_PERIOD_FRAMES: u32 = 2048;
/// How often kira updates clocks and modulators, in frames; kira's default.
const KIRA_INTERNAL_BUFFER_FRAMES: usize = 128;
/// How long a volume change takes, so moving a slider doesn't click.
const VOLUME_TWEEN: Duration = Duration::from_millis(50);

#[derive(Debug, thiserror::Error)]
pub enum OpenLocalMixerError {
    #[error("no default audio output device")]
    NoDevice(LocalOutputIdentity),
    #[error("the PipeWire output target {0:?} does not exist")]
    TargetNotFound(String),
    #[error("the default audio device reports no usable configuration")]
    DeviceConfiguration {
        identity: LocalOutputIdentity,
        #[source]
        source: cpal::Error,
    },
    #[error("the audio output stream cannot start")]
    Stream {
        identity: LocalOutputIdentity,
        #[source]
        source: cpal::Error,
    },
    #[error("the mixer cannot hold its tracks")]
    Tracks(#[source] ResourceLimitReached),
}

impl OpenLocalMixerError {
    pub(crate) fn reason(&self) -> LocalOutputFailureReason {
        match self {
            Self::NoDevice(_) => LocalOutputFailureReason::NoDevice,
            Self::TargetNotFound(_) => LocalOutputFailureReason::TargetMissing,
            Self::DeviceConfiguration { .. } | Self::Stream { .. } | Self::Tracks(_) => {
                LocalOutputFailureReason::OpenFailed
            }
        }
    }

    pub(crate) fn identity(&self) -> Option<&LocalOutputIdentity> {
        match self {
            Self::DeviceConfiguration { identity, .. } | Self::Stream { identity, .. } => {
                Some(identity)
            }
            Self::NoDevice(identity) => Some(identity),
            _ => None,
        }
    }
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

/// Initial track gains, applied at construction rather than interpolated from full volume.
#[derive(Debug, Clone, Copy)]
pub struct MixerVolumes {
    music: Decibels,
    effects: Decibels,
    layers: Decibels,
}

impl MixerVolumes {
    pub const SILENT: Self = Self {
        music: Decibels::SILENCE,
        effects: Decibels::SILENCE,
        layers: Decibels::SILENCE,
    };

    #[must_use]
    pub fn with_effects(effects: Volume) -> Self {
        Self {
            music: Decibels::IDENTITY,
            effects: decibels(effects.into_inner()),
            layers: Decibels::IDENTITY,
        }
    }
}

impl Default for MixerVolumes {
    fn default() -> Self {
        Self::with_effects(Volume::default())
    }
}

/// A kira mixer bound to one output, with the sample rate its tracks must be decoded at.
pub struct Mixer {
    manager: MixerManager,
    sample_rate: u32,
    music: MusicDeckHandle,
    music_layers: [(MusicDeckHandle, TrackHandle); MUSIC_LAYERS_EXTRA_MAX],
    music_track: TrackHandle,
    sounds_track: TrackHandle,
    browser_track: TrackHandle,
}

enum MixerManager {
    Local(Box<AudioManager<LocalBackend>>),
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
    pub fn open_local(volumes: MixerVolumes) -> Result<Self, OpenLocalMixerError> {
        Self::open_local_observed(volumes, Arc::new(LocalCallbackCounters::default()))
    }

    pub(crate) fn open_local_observed(
        volumes: MixerVolumes,
        callback_counters: Arc<LocalCallbackCounters>,
    ) -> Result<Self, OpenLocalMixerError> {
        #[cfg(target_os = "linux")]
        let pipewire = pipewire_output_device()?;
        #[cfg(target_os = "linux")]
        let device_override = pipewire.as_ref().map(|output| output.device.clone());
        #[cfg(not(target_os = "linux"))]
        let device_override: Option<cpal::Device> = None;
        let device =
            device_override.map_or_else(|| default_output_device(&cpal::default_host()), Ok)?;
        let mut identity = LocalOutputIdentity {
            backend: device
                .id()
                .map_or_else(|_| "unknown".to_owned(), |id| id.host().name().to_owned()),
            device: device.id().ok().map(|id| id.to_string()),
            sample_rate: None,
        };
        let supported = device.default_output_config().map_err(|source| {
            OpenLocalMixerError::DeviceConfiguration {
                identity: identity.clone(),
                source,
            }
        })?;
        let config = StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: BufferSize::Fixed(LOCAL_PERIOD_FRAMES),
        };
        identity.sample_rate = Some(config.sample_rate);
        tracing::info!(device = ?device.id(), sample_rate = config.sample_rate, "opening local audio output");
        let mut manager = AudioManager::<LocalBackend>::new(AudioManagerSettings {
            backend_settings: LocalBackendSettings {
                device,
                config,
                identity: identity.clone(),
                callback_counters,
            },
            main_track_builder: limited_main_track(),
            capacities: kira::Capacities::default(),
            internal_buffer_size: KIRA_INTERNAL_BUFFER_FRAMES,
        })
        .map_err(|source| OpenLocalMixerError::Stream { identity, source })?;
        let tracks = MixerTracks::add(&mut manager, config.sample_rate, volumes)
            .map_err(OpenLocalMixerError::Tracks)?;
        Ok(tracks.into_mixer(MixerManager::Local(Box::new(manager)), config.sample_rate))
    }

    /// Creates a mixer that renders only when the returned renderer is pulled.
    pub fn open_pulled(
        sample_rate: u32,
        volumes: MixerVolumes,
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
        let tracks = MixerTracks::add(&mut manager, sample_rate, volumes)
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
        manager.backend_mut().poll()
    }

    pub(crate) fn local_failure(&mut self, now: Instant) -> Option<LocalOutputFailure> {
        match &mut self.manager {
            MixerManager::Local(manager) => manager.backend_mut().failure(now),
            MixerManager::Pulled { .. } => None,
        }
    }

    pub(crate) fn local_identity(&mut self) -> LocalOutputIdentity {
        match &mut self.manager {
            MixerManager::Local(manager) => manager.backend_mut().identity().clone(),
            MixerManager::Pulled { .. } => LocalOutputIdentity {
                backend: "pulled".to_owned(),
                device: None,
                sample_rate: Some(self.sample_rate),
            },
        }
    }

    pub(crate) fn local_has_callback(&mut self) -> bool {
        match &mut self.manager {
            MixerManager::Local(manager) => manager.backend_mut().has_callback(),
            MixerManager::Pulled { .. } => true,
        }
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

    pub(crate) fn restore_music_layer_volume(&mut self, slot: usize, volume: f32) {
        if let Some((_, track)) = self.music_layers.get_mut(slot) {
            track.set_volume(decibels(volume), linear_tween(Duration::ZERO));
        }
    }

    /// Applies the saved gains before attaching audio to a replacement mixer.
    pub(crate) fn restore_volumes(&mut self, music: f32, effects: f32) {
        self.music_track
            .set_volume(decibels(music), linear_tween(Duration::ZERO));
        self.sounds_track
            .set_volume(decibels(effects), linear_tween(Duration::ZERO));
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

    pub fn play_static_sound(
        &mut self,
        sound: StaticSoundData,
    ) -> Result<StaticSoundHandle, MixerFullError> {
        self.sounds_track.play(sound).map_err(|_| MixerFullError)
    }

    pub fn play_effect(&mut self, sound: EffectSound) -> Result<(), MixerFullError> {
        self.sounds_track.play(sound).map_err(|_| MixerFullError)
    }

    /// Sets the browser's gain, a linear value from silence (0) to full level (1). The browser
    /// track starts silent.
    pub(crate) fn set_browser_volume(&mut self, volume: f32) {
        self.browser_track
            .set_volume(decibels(volume), linear_tween(VOLUME_TWEEN));
    }

    /// Adds an input for the browser's streams to the browser track. An input whose handle
    /// dropped leaves once its streams end.
    pub fn attach_browser_input(
        &mut self,
        counters: Arc<BrowserAudioCounters>,
    ) -> Result<BrowserInputHandle, MixerFullError> {
        let (sound, handle) = browser_input::browser_input(self.sample_rate, counters);
        self.browser_track.play(sound).map_err(|_| MixerFullError)?;
        Ok(handle)
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
    let target = std::env::var("PIPEWIRE_NODE").ok();
    let host = match cpal::host_from_id(cpal::HostId::PipeWire) {
        Ok(host) => host,
        Err(error) => {
            if let Some(target) = target {
                return Err(OpenLocalMixerError::TargetNotFound(target));
            }
            tracing::debug!(%error, "PipeWire is unavailable; using the platform audio host");
            return Ok(None);
        }
    };
    let device = target.map_or_else(
        || default_output_device(&host),
        |target| {
            let id = cpal::DeviceId::new(cpal::HostId::PipeWire, &target);
            host.device_by_id(&id)
                .filter(cpal::traits::DeviceTrait::supports_output)
                .ok_or(OpenLocalMixerError::TargetNotFound(target))
        },
    )?;
    Ok(Some(PipeWireOutputDevice {
        device,
        _host: host,
    }))
}

fn default_output_device(host: &cpal::Host) -> Result<cpal::Device, OpenLocalMixerError> {
    host.default_output_device().ok_or_else(|| {
        OpenLocalMixerError::NoDevice(LocalOutputIdentity {
            backend: host.id().name().to_owned(),
            device: None,
            sample_rate: None,
        })
    })
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
    browser_track: TrackHandle,
}

impl MixerTracks {
    fn add<B: Backend>(
        manager: &mut AudioManager<B>,
        sample_rate: u32,
        volumes: MixerVolumes,
    ) -> Result<Self, ResourceLimitReached> {
        let mut music_track = manager.add_sub_track(TrackBuilder::new().volume(volumes.music))?;
        let sounds_track = manager.add_sub_track(TrackBuilder::new().volume(volumes.effects))?;
        let browser_track = manager.add_sub_track(TrackBuilder::new().volume(Decibels::SILENCE))?;
        let (deck, music) = music_deck::music_deck(sample_rate);
        music_track.play(deck).map_err(|_| ResourceLimitReached)?;
        let mut add_layer = || {
            let (deck, handle) = music_deck::music_deck(sample_rate);
            let mut track =
                music_track.add_sub_track(TrackBuilder::new().volume(volumes.layers))?;
            track.play(deck).map_err(|_| ResourceLimitReached)?;
            Ok::<_, ResourceLimitReached>((handle, track))
        };
        let music_layers = [add_layer()?, add_layer()?];
        Ok(Self {
            music,
            music_layers,
            music_track,
            sounds_track,
            browser_track,
        })
    }

    fn into_mixer(self, manager: MixerManager, sample_rate: u32) -> Mixer {
        Mixer {
            manager,
            sample_rate,
            music: self.music,
            music_layers: self.music_layers,
            music_track: self.music_track,
            sounds_track: self.sounds_track,
            browser_track: self.browser_track,
        }
    }
}
