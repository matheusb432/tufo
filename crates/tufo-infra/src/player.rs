//! The playback engine behind the application's `Player` port.
//!
//! One tokio mutex guards the engine's state. Commands hold it briefly; reading and decoding
//! files runs on blocking threads outside it, and a music change applies only if no later change
//! superseded it meanwhile. A tick every [`TICK_INTERVAL`] follows the deck: it records the track
//! changes the deck made on its own, arms the next track ahead of time, clears finished sounds,
//! and lets go of the output once nothing plays. The [`browser`] host's pages play on the same
//! output.

mod browser;
mod layers;
mod local_output;
mod music;
pub mod outputs;
mod preview;
mod sounds;

use std::{
    collections::BTreeMap,
    convert::Infallible,
    hash::{BuildHasher as _, Hasher as _, RandomState},
    num::NonZeroU64,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use browser::BrowserSession;
pub use browser::BrowserTimings;
use layers::MusicLayer;
use local_output::LocalReopens;
use music::{
    Music, MusicTrack, OpenedTrack, TrackSource, open_track, play_audio_error, probe_track,
    start_position,
};
use outputs::{AudioOutputs, DeviceOutputs, OpenedOutput};
use sounds::{FiredSound, volume_gain};
use tokio::sync::{Mutex, mpsc::UnboundedSender, oneshot, watch};
use tufo_application::playback::{
    add_music_layer::{AddMusicLayerError, MusicLayerCue},
    add_to_queue::QueueSongError,
    control_browser::BrowserCommand,
    control_music::{ControlMusicError, MusicCommand},
    control_sounds::SoundsCommand,
    open_browser::OpenBrowserError,
    play_queue::PlayQueueError,
    player::{PlayAudioError, Player, PreviewId, PreviewSession, SongLoops, SongPreview},
    recover_local_output::RecoverLocalOutputError,
    select_output::SelectOutputError,
};
use tufo_models::{
    library::{CollectionEntryId, SoundSettings, Volume, song_loop::LoopRegion},
    playback::{
        AudioFilePath, OutputTarget,
        browser::BrowserStatus,
        diagnostics::PlayerDiagnostics,
        local_output::LocalOutputState,
        music_layers::MusicLayers,
        player_state::{
            CrossfadeDuration, MusicState, PlayerState, PositionSample, SoundCue,
            VoiceConnectionState,
        },
        queue::{
            EnqueueError, MusicQueue, QueueEntry, QueueModes, QueueStart, QueuedEntry, RepeatMode,
            SetRepeatError,
        },
    },
};

use crate::{
    audio::{
        LocalStreamErrors, MixerVolumes,
        browser_input::BrowserAudioCounters,
        buffered_track::{BufferedTrack, DecodeOptions},
        effect_sound::{EffectSource, effect_sound},
        music_deck::{DeckState, DeckTrack, MusicDeckHandle},
        sound_cache::{LoadedSound, SoundCache, SoundKey, SoundLoadOptions, load_sound},
    },
    browser::{BrowserLauncher, BrowserNotice},
    discord::{SongbirdDiscordClient, VoiceAvailability},
};

const TICK_INTERVAL: Duration = Duration::from_millis(50);
/// How long before a track's crossfade point the next track starts decoding.
const ARM_LEAD: Duration = Duration::from_secs(10);
/// Skipping fades over the crossfade, but never longer than this.
const MANUAL_TRANSITION_MAX: Duration = Duration::from_secs(1);
/// The shortest fade, which only keeps a cut from clicking.
const ANTI_CLICK_FADE: Duration = Duration::from_millis(20);
const PAUSE_FADE: Duration = Duration::from_millis(100);
const STOP_FADE: Duration = Duration::from_millis(250);
/// How music fades back in after moving to another output.
const RESTORE_FADE_IN: Duration = Duration::from_millis(50);
/// About three minutes of 48 kHz audio; longer loops stream.
const SOUND_CACHE_BYTES_MAX: usize = 32 * 1024 * 1024;
const SOUND_CACHE_IDLE_MAX: Duration = Duration::from_mins(10);

/// How the engine paces what it does on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineTimings {
    pub reconnect_policy: ReconnectPolicy,
    /// How long the local device stays open with nothing playing before the engine closes it and
    /// drops its decoded sounds.
    pub idle_release_delay: Duration,
    pub browser: BrowserTimings,
}

impl Default for EngineTimings {
    fn default() -> Self {
        Self {
            reconnect_policy: ReconnectPolicy::default(),
            idle_release_delay: Duration::from_mins(5),
            browser: BrowserTimings::default(),
        }
    }
}

/// How the engine joins a voice channel again after its connection drops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub attempts_max: u32,
    /// The wait before the first attempt, doubling after each failure.
    pub delay_min: Duration,
    pub delay_max: Duration,
}

impl Default for ReconnectPolicy {
    /// Eight attempts over about two minutes.
    fn default() -> Self {
        Self {
            attempts_max: 8,
            delay_min: Duration::from_secs(1),
            delay_max: Duration::from_secs(30),
        }
    }
}

/// Plays music, sound effects, and the browser's pages on the local device or one Discord voice
/// channel.
pub struct EnginePlayer<O: AudioOutputs> {
    outputs: O,
    timings: EngineTimings,
    song_loops: Box<dyn SongLoops>,
    browser_launcher: Arc<dyn BrowserLauncher>,
    /// Serializes opening and closing the browser, which wait outside the engine lock.
    browser_lifecycle: Mutex<()>,
    browser_notices: UnboundedSender<BrowserNotice>,
    browser_counters: Arc<BrowserAudioCounters>,
    engine: Mutex<Engine>,
    state: watch::Sender<PlayerState>,
    /// Updated by the tick, so diagnostics never wait for the engine lock.
    music_underrun_frames: AtomicU64,
    sound_underrun_frames: Arc<AtomicU64>,
    local_stream_errors: AtomicU64,
    local_stream_errors_discarded: AtomicU64,
    this: Weak<Self>,
}

impl EnginePlayer<DeviceOutputs> {
    /// Starts on the local output. Must run inside a tokio runtime.
    #[must_use]
    pub fn start(
        discord: SongbirdDiscordClient,
        song_loops: impl SongLoops,
        browser_launcher: impl BrowserLauncher,
    ) -> Arc<Self> {
        Self::with_outputs(
            DeviceOutputs::new(discord),
            EngineTimings::default(),
            song_loops,
            browser_launcher,
        )
    }
}

impl<O: AudioOutputs> EnginePlayer<O> {
    /// Starts on the local output of `outputs`, which opens on first use. Must run inside a
    /// tokio runtime.
    #[must_use]
    pub fn with_outputs(
        outputs: O,
        timings: EngineTimings,
        song_loops: impl SongLoops,
        browser_launcher: impl BrowserLauncher,
    ) -> Arc<Self> {
        let sound_underrun_frames = Arc::new(AtomicU64::new(0));
        let engine = Engine {
            target: OutputTarget::Local,
            output: None,
            local_output: LocalOutputState::Released,
            local_reopens: LocalReopens::default(),
            output_generation: 0,
            voice: idle_voice(outputs.voice_availability(), None),
            music: None,
            music_layers: MusicLayers::default(),
            music_generation: 0,
            planned_queue: None,
            volume: Volume::default(),
            effects_volume: Volume::default(),
            muted: false,
            modes: QueueModes::default(),
            crossfade: CrossfadeDuration::default(),
            sounds: BTreeMap::new(),
            sounds_paused_at: None,
            sound_cache: SoundCache::new(SOUND_CACHE_BYTES_MAX, SOUND_CACHE_IDLE_MAX),
            sound_underrun_frames: Arc::clone(&sound_underrun_frames),
            local_stream_errors_retired: LocalStreamErrors::default(),
            last_token: 0,
            retired_underrun_frames: 0,
            release_at: None,
            preview: None,
            last_preview_id: 0,
            browser: BrowserSession::new(browser_launcher.is_available()),
        };
        let state = watch::Sender::new(engine.snapshot());
        let (browser_notices, browser_notice_receiver) = tokio::sync::mpsc::unbounded_channel();
        let player = Arc::new_cyclic(|this| Self {
            outputs,
            timings,
            song_loops: Box::new(song_loops),
            browser_launcher: Arc::new(browser_launcher),
            browser_lifecycle: Mutex::new(()),
            browser_notices,
            browser_counters: Arc::default(),
            engine: Mutex::new(engine),
            state,
            music_underrun_frames: AtomicU64::new(0),
            sound_underrun_frames,
            local_stream_errors: AtomicU64::new(0),
            local_stream_errors_discarded: AtomicU64::new(0),
            this: this.clone(),
        });
        tokio::spawn(run_ticks(Arc::downgrade(&player)));
        tokio::spawn(browser::run_browser_notices(
            Arc::downgrade(&player),
            browser_notice_receiver,
        ));
        player
    }

    /// Stops all audio, ends the browser, leaves any voice channel so the bot disappears from it
    /// immediately, and logs the playback diagnostics.
    pub async fn shut_down(&self) {
        self.close_browser_host().await;
        let mut engine = self.engine.lock().await;
        self.discard_preview(&mut engine).await;
        engine.music_generation += 1;
        if let Some(music) = engine.music.take() {
            engine.retire_music(&music);
        }
        engine.stop_layers();
        engine.sounds.clear();
        engine.output_generation += 1;
        if let Some(output) = engine.take_output() {
            self.outputs.close(output).await;
        }
        self.update_diagnostics(&mut engine);
        self.publish(&engine);
        tracing::info!(diagnostics = ?self.diagnostics(), "playback diagnostics at shutdown");
    }

    fn publish(&self, engine: &Engine) {
        self.state.send_replace(engine.snapshot());
    }

    fn update_diagnostics(&self, engine: &mut Engine) {
        self.music_underrun_frames
            .store(engine.underrun_frames(), Ordering::Relaxed);
        let errors = engine.poll_local_stream_errors();
        self.local_stream_errors
            .store(errors.reported, Ordering::Relaxed);
        self.local_stream_errors_discarded
            .store(errors.discarded, Ordering::Relaxed);
    }

    /// Opens the local output when nothing is open yet. A voice output that dropped stays
    /// closed until it reconnects.
    async fn ensure_output(&self, engine: &mut Engine) -> Result<(), PlayAudioError> {
        // Audio starts on the output next, which a pending release must not close meanwhile.
        engine.release_at = None;
        if engine.output.is_some() {
            return Ok(());
        }
        if let OutputTarget::DiscordVoice(_) = engine.target {
            return Err(PlayAudioError::OutputUnavailable(anyhow::anyhow!(
                "the Discord voice connection is down"
            )));
        }
        if matches!(
            engine.local_output,
            LocalOutputState::Failed(_) | LocalOutputState::Recovering { .. }
        ) {
            return Err(PlayAudioError::OutputUnavailable(anyhow::anyhow!(
                "recover the local output before starting playback"
            )));
        }
        engine.local_output = LocalOutputState::Recovering {
            previous_failure: None,
        };
        self.publish(engine);
        let opened = match self
            .outputs
            .open(
                OutputTarget::Local,
                MixerVolumes::with_effects(engine.effects_volume),
            )
            .await
        {
            Ok(opened) => opened,
            Err(error) => {
                engine.record_local_open_failure(&error);
                self.publish(engine);
                return Err(PlayAudioError::OutputUnavailable(error.into()));
            }
        };
        self.install_output(engine, opened).await;
        self.publish(engine);
        Ok(())
    }

    /// Makes `opened` the engine's output and plays the music and looping sounds on it again.
    async fn install_output(&self, engine: &mut Engine, mut opened: OpenedOutput) {
        engine.output_generation += 1;
        // A release planned for the previous output, such as the idle one, must not close this one.
        engine.release_at = None;
        // Music changes planned for the previous mixer's sample rate must not apply here.
        engine.music_generation += 1;
        if let Some(voice_lost) = opened.take_voice_lost() {
            self.watch_voice_loss(voice_lost, engine.output_generation);
        }
        opened.mixer.restore_volumes(
            volume_gain(engine.volume, engine.muted),
            engine.effects_volume.into_inner(),
        );
        engine.local_output = match engine.target {
            OutputTarget::Local => LocalOutputState::Ready(opened.mixer.local_identity()),
            OutputTarget::DiscordVoice(_) => LocalOutputState::Released,
        };
        if let Err(error) = opened
            .mixer
            .music()
            .set_crossfade(engine.crossfade.into_inner())
        {
            tracing::warn!(%error, "setting the crossfade on a new mixer failed");
        }
        if engine.sounds_paused_at.is_some() {
            opened.mixer.pause_sounds(Duration::ZERO);
        }
        let sample_rate = opened.mixer.sample_rate();
        engine.output = Some(opened);
        self.attach_browser(engine);
        self.restore_music(engine, sample_rate).await;
        self.restore_layers(engine, sample_rate).await;
        self.restore_sounds(engine).await;
    }

    /// Closes the output, keeping the music's position and the looping sounds to play again on
    /// the next one.
    async fn suspend_output(&self, engine: &mut Engine) {
        if let Some(output) = engine.detach_output() {
            self.outputs.close(output).await;
        }
    }

    async fn restore_music(&self, engine: &mut Engine, sample_rate: u32) {
        let Some(music) = &engine.music else {
            return;
        };
        let start_offset = music.current.position();
        let source = TrackSource::Open(music.current.file.clone());
        let loop_region = music.current.loop_region;
        let options = DecodeOptions {
            start_position: start_offset,
            looping: engine.main_track_loops(loop_region),
            loop_region,
        };
        let opened = open_track(source, sample_rate, options).await;
        let token = engine.next_token();
        let (Some(music), Some(output)) = (engine.music.as_mut(), engine.output.as_mut()) else {
            return;
        };
        let restored = opened.map_err(anyhow::Error::from).and_then(|opened| {
            let deck = output.mixer.music();
            if music.paused {
                deck.pause(Duration::ZERO)?;
            }
            deck.start(
                DeckTrack {
                    token,
                    reader: opened.track.reader,
                },
                RESTORE_FADE_IN,
                Duration::ZERO,
            )?;
            Ok(MusicTrack {
                token,
                file: opened.file,
                start_offset,
                duration: music.current.duration,
                loop_region,
                handle: Some(opened.track.handle),
            })
        });
        match restored {
            Ok(current) => music.current = current,
            Err(error) => {
                tracing::warn!(
                    error = format!("{error:#}"),
                    "the music cannot resume on the new output"
                );
                engine.music = None;
            }
        }
    }

    async fn restore_sounds(&self, engine: &mut Engine) {
        let cues = engine
            .sounds
            .values()
            .filter(|sound| sound.playback.is_none())
            .map(|sound| sound.cue.clone())
            .collect::<Vec<_>>();
        for cue in cues {
            let entry_id = cue.entry_id;
            let fired = match self.load_sound_cached(engine, &cue).await {
                Ok(loaded) => fire_sound(engine, cue, loaded).await,
                Err(error) => Err(error),
            };
            match fired {
                Ok(fired) => {
                    engine.sounds.insert(entry_id, fired);
                }
                Err(error) => {
                    tracing::warn!(%error, "a looping sound cannot resume on the new output");
                    engine.sounds.remove(&entry_id);
                }
            }
        }
    }

    /// Loads a sound through the cache while holding the engine, for restoring loops.
    async fn load_sound_cached(
        &self,
        engine: &mut Engine,
        cue: &SoundCue,
    ) -> Result<LoadedSound, PlayAudioError> {
        let key = read_sound_key(&cue.path).await?;
        if let Some(sound) = engine.sound_cache.get(&key, Instant::now()) {
            return Ok(LoadedSound::Decoded(sound));
        }
        let loaded = load_sound_blocking(&cue.path, cue.settings.looping).await?;
        if let LoadedSound::Decoded(sound) = &loaded {
            engine
                .sound_cache
                .insert(key, sound.clone(), Instant::now());
        }
        Ok(loaded)
    }

    /// Marks a change of the current track and describes it, for [`Self::apply_music_plan`].
    /// `queue` waits in the engine meanwhile, so queue edits reach it.
    fn plan_music(
        engine: &mut Engine,
        queue: MusicQueue,
        change: MusicChange,
    ) -> Result<MusicPlan, PlayAudioError> {
        let sample_rate = engine
            .output
            .as_ref()
            .map(|output| output.mixer.sample_rate())
            .ok_or_else(|| {
                PlayAudioError::OutputUnavailable(anyhow::anyhow!("no output is open"))
            })?;
        engine.music_generation += 1;
        if let Some(music) = &mut engine.music {
            music.changing = true;
        }
        engine.planned_queue = Some(PlannedQueue {
            generation: engine.music_generation,
            queue,
        });
        Ok(MusicPlan {
            generation: engine.music_generation,
            change,
            sample_rate,
        })
    }

    /// Opens the planned track and makes it current, unless a later change superseded it.
    async fn apply_music_plan(&self, plan: MusicPlan) -> Result<(), PlayAudioError> {
        let MusicChange { source, start } = plan.change;
        let options = DecodeOptions {
            start_position: start.start_offset,
            looping: start.looping,
            loop_region: start.loop_region,
        };
        let opened = open_track(source, plan.sample_rate, options).await;
        let mut engine = self.engine.lock().await;
        if engine.music_generation != plan.generation {
            return Ok(());
        }
        let planned = engine.planned_queue.take().map(|planned| planned.queue);
        let (opened, queue) = match (opened, planned) {
            (Ok(opened), Some(queue)) => (opened, queue),
            (opened, _) => {
                if let Some(music) = &mut engine.music {
                    music.changing = false;
                }
                return opened.map(drop);
            }
        };
        Self::install_music_track(&mut engine, queue, opened, start)?;
        self.publish(&engine);
        Ok(())
    }

    fn install_music_track(
        engine: &mut Engine,
        queue: MusicQueue,
        opened: OpenedTrack,
        start: TrackStart,
    ) -> Result<(), PlayAudioError> {
        let TrackStart {
            start_offset,
            fade_in,
            fade_out,
            paused,
            loop_region,
            looping,
        } = start;
        engine.pause_layers(paused)?;
        let token = engine.next_token();
        let OpenedTrack {
            track,
            file,
            duration,
        } = opened;
        let deck = engine.deck().ok_or_else(|| {
            PlayAudioError::OutputUnavailable(anyhow::anyhow!("the output closed"))
        })?;
        let resumed = if paused {
            Ok(())
        } else {
            deck.resume(ANTI_CLICK_FADE)
        };
        resumed
            .and_then(|()| {
                deck.start(
                    DeckTrack {
                        token,
                        reader: track.reader,
                    },
                    fade_in,
                    fade_out,
                )
            })
            .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?;
        let looping_now = engine.main_track_loops(loop_region);
        if looping_now != looping {
            track.handle.set_looping(looping_now);
        }
        let current = MusicTrack {
            token,
            file,
            start_offset,
            duration: duration.or(queue.current().duration),
            loop_region,
            handle: Some(track.handle),
        };
        let previous = engine.music.replace(Music {
            queue,
            current,
            armed: None,
            paused,
            changing: false,
            arm_requested: false,
        });
        if let Some(previous) = previous {
            engine.retire_music(&previous);
        }
        Ok(())
    }

    /// Starts `entry` from its playback start with the region saved for it now.
    fn song_start(
        &self,
        engine: &Engine,
        entry: &QueueEntry,
        fade: Duration,
        paused: bool,
    ) -> TrackStart {
        let loop_region = self.song_loops.loop_region(entry.audio_file_id);
        TrackStart {
            start_offset: start_position(loop_region),
            fade_in: fade,
            fade_out: fade,
            paused,
            loop_region,
            looping: engine.main_track_loops(loop_region),
        }
    }

    /// Plans moving the queue to its next or previous track, or stops at its end.
    async fn skip(&self, backwards: bool) -> Result<(), ControlMusicError> {
        let mut engine = self.engine.lock().await;
        let music = engine
            .music
            .as_ref()
            .ok_or(ControlMusicError::NothingPlaying)?;
        let mut queue = music.queue.clone();
        let current_entry_id = queue.current().entry_id;
        let next = if backwards {
            queue.skip_to_previous(music.current.elapsed())
        } else {
            queue.skip_to_next()
        }
        .cloned();
        let Some(next) = next else {
            engine.stop_music();
            self.publish(&engine);
            return Ok(());
        };
        let source = if next.entry_id == current_entry_id {
            TrackSource::Open(music.current.file.clone())
        } else {
            TrackSource::Path(next.path.clone())
        };
        let fade = engine.manual_fade();
        let start = self.song_start(&engine, &next, fade, false);
        self.ensure_output(&mut engine).await?;
        let plan = Self::plan_music(&mut engine, queue, MusicChange { source, start })?;
        drop(engine);
        Ok(self.apply_music_plan(plan).await?)
    }

    async fn seek(&self, position: Duration) -> Result<(), ControlMusicError> {
        let mut engine = self.engine.lock().await;
        let music = engine
            .music
            .as_ref()
            .ok_or(ControlMusicError::NothingPlaying)?;
        let loop_region = music.current.loop_region;
        let position = match loop_region {
            Some(region) => region.clamp_start(position),
            None => Some(position),
        }
        .filter(|position| {
            music
                .current
                .duration
                .is_none_or(|duration| *position < duration)
        })
        .ok_or(ControlMusicError::SeekPastEnd)?;
        let queue = music.queue.clone();
        let change = MusicChange {
            source: TrackSource::Open(music.current.file.clone()),
            start: TrackStart {
                start_offset: position,
                fade_in: ANTI_CLICK_FADE,
                fade_out: ANTI_CLICK_FADE,
                paused: music.paused,
                loop_region,
                looping: engine.main_track_loops(loop_region),
            },
        };
        self.ensure_output(&mut engine).await?;
        let plan = Self::plan_music(&mut engine, queue, change)?;
        drop(engine);
        Ok(self.apply_music_plan(plan).await?)
    }

    /// Follows the queue after the current track ended with nothing armed.
    async fn advance_after_end(self: Arc<Self>, generation: u64) {
        let plan = {
            let mut engine = self.engine.lock().await;
            if engine.music_generation != generation {
                return;
            }
            let Some(music) = &engine.music else {
                return;
            };
            let mut queue = music.queue.clone();
            let current_entry_id = queue.current().entry_id;
            let paused = music.paused;
            let Some(next) = queue.advance().cloned() else {
                engine.stop_music();
                self.publish(&engine);
                return;
            };
            let source = if next.entry_id == current_entry_id {
                TrackSource::Open(music.current.file.clone())
            } else {
                TrackSource::Path(next.path.clone())
            };
            let change = MusicChange {
                source,
                start: self.song_start(&engine, &next, Duration::ZERO, paused),
            };
            match Self::plan_music(&mut engine, queue, change) {
                Ok(plan) => plan,
                Err(_) => return,
            }
        };
        if let Err(error) = self.apply_music_plan(plan).await {
            tracing::warn!(%error, "the next track cannot play, so the music stops");
            let mut engine = self.engine.lock().await;
            engine.stop_music();
            self.publish(&engine);
        }
    }

    /// Decodes the queue's following track and hands it to the deck ahead of time.
    async fn arm_following(self: Arc<Self>, request: ArmRequest) {
        let options = DecodeOptions {
            start_position: start_position(request.loop_region),
            looping: request.looping,
            loop_region: request.loop_region,
        };
        let opened = open_track(request.source, request.sample_rate, options).await;
        let mut engine = self.engine.lock().await;
        let token = engine.next_token();
        let still_wanted = engine.music.as_ref().is_some_and(|music| {
            music.current.token == request.current_token
                && music.armed.is_none()
                && music.queue.following().map(|entry| entry.entry_id) == Some(request.entry_id)
        });
        if !still_wanted {
            return;
        }
        let opened = match opened {
            Ok(opened) => opened,
            Err(error) => {
                tracing::warn!(%error, "the next track cannot be prepared");
                return;
            }
        };
        let Some(deck) = engine.deck() else {
            return;
        };
        let armed = deck.arm(DeckTrack {
            token,
            reader: opened.track.reader,
        });
        if let Err(error) = armed {
            tracing::warn!(%error, "arming the next track failed");
            return;
        }
        if let Some(music) = &mut engine.music {
            let duration = opened
                .duration
                .or_else(|| music.queue.following().and_then(|entry| entry.duration));
            music.armed = Some(MusicTrack {
                token,
                file: opened.file,
                start_offset: start_position(request.loop_region),
                duration,
                loop_region: request.loop_region,
                handle: Some(opened.track.handle),
            });
        }
    }

    async fn tick(self: &Arc<Self>) {
        let mut engine = self.engine.lock().await;
        let mut changed = self.poll_local_output(&mut engine);
        changed |= engine.reap_finished_sounds();
        engine.sound_cache.evict_idle(Instant::now());
        changed |= engine.reap_finished_layers();
        if let Some(deck) = engine.deck() {
            deck.drop_retired();
        }
        changed |= self.follow_deck(&mut engine);
        self.arm_if_due(&mut engine);
        changed |= self.release_if_due(&mut engine).await;
        changed |= self.release_browser_if_due(&mut engine);
        self.update_diagnostics(&mut engine);
        if changed {
            self.publish(&engine);
        }
    }

    /// Records a track change the deck made, or follows the queue once the current track ended.
    fn follow_deck(self: &Arc<Self>, engine: &mut Engine) -> bool {
        let Engine {
            output,
            music,
            music_generation,
            retired_underrun_frames,
            ..
        } = engine;
        let (Some(output), Some(music)) = (output.as_mut(), music.as_mut()) else {
            return false;
        };
        if music.changing {
            return false;
        }
        match output.mixer.music().state() {
            DeckState::Playing(token)
                if music
                    .armed
                    .as_ref()
                    .is_some_and(|armed| armed.token == token) =>
            {
                let Some(armed) = music.armed.take() else {
                    return false;
                };
                let finished = std::mem::replace(&mut music.current, armed);
                *retired_underrun_frames += finished.underrun_frames();
                music.queue.advance();
                music.arm_requested = false;
                true
            }
            // Either the current track still plays, or commands haven't reached the deck yet.
            DeckState::Pending | DeckState::Playing(_) => false,
            DeckState::Idle => {
                music.changing = true;
                tokio::spawn(Arc::clone(self).advance_after_end(*music_generation));
                false
            }
        }
    }

    fn arm_if_due(self: &Arc<Self>, engine: &mut Engine) {
        let crossfade = engine.crossfade.into_inner();
        let modes = engine.modes;
        let (Some(output), Some(music)) = (engine.output.as_ref(), engine.music.as_mut()) else {
            return;
        };
        if music.changing
            || music.paused
            || music.armed.is_some()
            || music.arm_requested
            || main_track_loops(modes, music.current.loop_region)
        {
            return;
        }
        let Some(following) = music.queue.following() else {
            return;
        };
        let remaining = music
            .current
            .end()
            .map(|end| end.saturating_sub(music.current.position()));
        if remaining.is_some_and(|remaining| remaining > ARM_LEAD + crossfade) {
            return;
        }
        let source = if following.entry_id == music.queue.current().entry_id {
            TrackSource::Open(music.current.file.clone())
        } else {
            TrackSource::Path(following.path.clone())
        };
        let loop_region = self.song_loops.loop_region(following.audio_file_id);
        let request = ArmRequest {
            current_token: music.current.token,
            entry_id: following.entry_id,
            source,
            sample_rate: output.mixer.sample_rate(),
            loop_region,
            looping: main_track_loops(modes, loop_region),
        };
        music.arm_requested = true;
        tokio::spawn(Arc::clone(self).arm_following(request));
    }

    /// Releases the audio once nothing has played on the local device for the idle delay, or once
    /// a stand-by's fade-out has ended, and reports whether it did.
    async fn release_if_due(&self, engine: &mut Engine) -> bool {
        if engine.has_audio() {
            engine.release_at = None;
            return false;
        }
        let now = Instant::now();
        if engine.release_at.is_none()
            && engine.output.is_some()
            && engine.target == OutputTarget::Local
        {
            engine.release_at = Some(now + self.timings.idle_release_delay);
        }
        if engine.release_at.is_none_or(|release_at| release_at > now) {
            return false;
        }
        engine.release_at = None;
        // Also ends reconnection attempts to a voice channel.
        engine.output_generation += 1;
        if let Some(output) = engine.take_output() {
            self.outputs.close(output).await;
            engine.local_output = LocalOutputState::Released;
        }
        if let OutputTarget::DiscordVoice(_) = engine.target {
            engine.target = OutputTarget::Local;
            engine.voice = idle_voice(self.outputs.voice_availability(), None);
        }
        engine.sound_cache.clear();
        true
    }

    fn watch_voice_loss(&self, voice_lost: oneshot::Receiver<String>, output_generation: u64) {
        let this = self.this.clone();
        tokio::spawn(async move {
            let Ok(reason) = voice_lost.await else {
                return;
            };
            if let Some(player) = this.upgrade() {
                player.reconnect(output_generation, reason).await;
            }
        });
    }

    /// Joins the lost voice channel again with growing delays, then gives up with the last
    /// reason. Selecting another output meanwhile ends the attempts.
    async fn reconnect(&self, lost_generation: u64, reason: String) {
        let generation = {
            let mut engine = self.engine.lock().await;
            if engine.output_generation != lost_generation {
                return;
            }
            tracing::warn!(reason, "the voice connection dropped; joining again");
            self.suspend_output(&mut engine).await;
            engine.voice = VoiceConnectionState::Reconnecting { attempt: 1 };
            self.publish(&engine);
            engine.output_generation
        };
        let mut last_reason = reason;
        let mut delay = self.timings.reconnect_policy.delay_min;
        for attempt in 1..=self.timings.reconnect_policy.attempts_max {
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(self.timings.reconnect_policy.delay_max);
            let mut engine = self.engine.lock().await;
            if engine.output_generation != generation {
                return;
            }
            engine.voice = VoiceConnectionState::Reconnecting { attempt };
            self.publish(&engine);
            match self
                .outputs
                .open(
                    engine.target,
                    MixerVolumes::with_effects(engine.effects_volume),
                )
                .await
            {
                Ok(opened) => {
                    engine.voice = VoiceConnectionState::Connected;
                    self.install_output(&mut engine, opened).await;
                    self.publish(&engine);
                    tracing::info!(attempt, "joined the voice channel again");
                    return;
                }
                Err(error) => last_reason = error_chain(&error),
            }
        }
        let mut engine = self.engine.lock().await;
        if engine.output_generation == generation {
            tracing::error!(
                reason = last_reason,
                "gave up joining the voice channel again"
            );
            engine.voice = VoiceConnectionState::Disconnected {
                reason: Some(last_reason),
            };
            self.publish(&engine);
        }
    }
}

impl<O: AudioOutputs> Player for EnginePlayer<O> {
    async fn recover_local_output(&self) -> Result<(), RecoverLocalOutputError> {
        self.start_local_recovery().await
    }

    async fn add_music_layer(&self, cue: MusicLayerCue) -> Result<(), AddMusicLayerError> {
        self.add_layer(cue).await
    }

    async fn stop_music_layer(&self, entry_id: CollectionEntryId) {
        let mut engine = self.engine.lock().await;
        if engine
            .music
            .as_ref()
            .is_some_and(|music| music.queue.current().entry_id == entry_id)
        {
            engine.stop_music();
        } else {
            engine.stop_layer(entry_id);
        }
        self.publish(&engine);
    }

    async fn update_music_layer(
        &self,
        entry_id: CollectionEntryId,
        update: tufo_wire::player::UpdateMusicLayer,
    ) {
        let mut engine = self.engine.lock().await;
        engine.update_layer(entry_id, update);
        self.publish(&engine);
    }

    async fn play_queue(&self, start: QueueStart) -> Result<(), PlayQueueError> {
        // Probing the start track first reports a bad file without opening the audio device.
        let start_file = match start.entries.get(start.start_index) {
            Some(entry) => Some(probe_track(entry.path.clone()).await?),
            None => None,
        };
        let mut engine = self.engine.lock().await;
        let queue = MusicQueue::new(start, engine.modes, shuffle_seed())?;
        // Shuffling keeps the start track current, so the file probed above belongs to it.
        let source = match start_file {
            Some(file) => TrackSource::Open(file),
            None => TrackSource::Path(queue.current().path.clone()),
        };
        let fade = engine.manual_fade();
        let start = self.song_start(&engine, queue.current(), fade, false);
        self.ensure_output(&mut engine).await?;
        let plan = Self::plan_music(&mut engine, queue, MusicChange { source, start })?;
        drop(engine);
        Ok(self.apply_music_plan(plan).await?)
    }

    async fn add_to_queue(&self, song: QueuedEntry) -> Result<(), QueueSongError> {
        let mut engine = self.engine.lock().await;
        let following = engine.following_entry_id();
        engine
            .edit_queues(|queue| queue.enqueue(song.clone()))
            .ok_or(QueueSongError::NothingPlaying)?
            .map_err(|EnqueueError::RepeatingTrack| QueueSongError::RepeatingTrack)?;
        engine.disarm_unless_following(following);
        self.publish(&engine);
        Ok(())
    }

    async fn remove_from_queue(&self, entry_id: CollectionEntryId) {
        let mut engine = self.engine.lock().await;
        let following = engine.following_entry_id();
        engine.edit_queues(|queue| {
            queue.remove_queued(entry_id);
            Ok::<_, Infallible>(())
        });
        engine.disarm_unless_following(following);
        self.publish(&engine);
    }

    async fn control_music(&self, command: MusicCommand) -> Result<(), ControlMusicError> {
        match command {
            MusicCommand::Next => return self.skip(false).await,
            MusicCommand::Previous => return self.skip(true).await,
            MusicCommand::Seek(position) => return self.seek(position).await,
            _ => {}
        }
        let mut engine = self.engine.lock().await;
        if matches!(command, MusicCommand::Pause | MusicCommand::Resume)
            && let Some(preview) = &mut engine.preview
        {
            preview.resumes_music = false;
        }
        match command {
            MusicCommand::Pause => engine.set_paused(true)?,
            MusicCommand::Resume => engine.set_paused(false)?,
            MusicCommand::Stop => {
                engine.stop_music();
                engine.stop_layers();
            }
            MusicCommand::SetVolume(volume) => {
                engine.volume = volume;
                engine.apply_music_volume();
            }
            MusicCommand::SetMuted(muted) => {
                engine.muted = muted;
                engine.apply_music_volume();
            }
            MusicCommand::SetShuffle(shuffle) => {
                engine.modes.shuffle = shuffle;
                let seed = shuffle_seed();
                engine.edit_queues(|queue| {
                    queue.set_shuffle(shuffle, seed);
                    Ok::<_, Infallible>(())
                });
                engine.disarm();
            }
            MusicCommand::SetRepeat(repeat) => {
                if let Some(edited) = engine.edit_queues(|queue| queue.set_repeat(repeat)) {
                    edited.map_err(|SetRepeatError::SongsQueued| ControlMusicError::SongsQueued)?;
                }
                engine.modes.repeat = repeat;
                engine.disarm();
                engine.update_main_track_looping();
            }
            MusicCommand::SetCrossfade(crossfade) => {
                engine.crossfade = crossfade;
                if let Some(deck) = engine.deck() {
                    deck.set_crossfade(crossfade.into_inner())
                        .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?;
                }
            }
            MusicCommand::Next | MusicCommand::Previous | MusicCommand::Seek(_) => {}
        }
        self.publish(&engine);
        Ok(())
    }

    async fn play_sound(&self, cue: SoundCue) -> Result<(), PlayAudioError> {
        let key = read_sound_key(&cue.path).await?;
        let cached = self
            .engine
            .lock()
            .await
            .sound_cache
            .get(&key, Instant::now());
        let loaded = match cached {
            Some(sound) => LoadedSound::Decoded(sound),
            None => load_sound_blocking(&cue.path, cue.settings.looping).await?,
        };
        let mut engine = self.engine.lock().await;
        if let LoadedSound::Decoded(sound) = &loaded {
            engine
                .sound_cache
                .insert(key, sound.clone(), Instant::now());
        }
        self.ensure_output(&mut engine).await?;
        if let Some(mut previous) = engine.sounds.remove(&cue.entry_id)
            && let Some(playback) = &mut previous.playback
        {
            playback.stop(engine.sound_stop_fade(ANTI_CLICK_FADE));
        }
        engine.set_sounds_paused(false);
        let entry_id = cue.entry_id;
        let fired = fire_sound(&mut engine, cue, loaded).await?;
        engine.sounds.insert(entry_id, fired);
        self.publish(&engine);
        Ok(())
    }

    async fn stop_sound(&self, entry_id: CollectionEntryId) {
        let mut engine = self.engine.lock().await;
        let Some(mut sound) = engine.sounds.remove(&entry_id) else {
            return;
        };
        if let Some(playback) = &mut sound.playback {
            playback.stop(engine.sound_stop_fade(sound.cue.settings.fade_out.into_inner()));
        }
        engine.unpause_when_no_sounds_left();
        self.publish(&engine);
    }

    async fn control_sounds(&self, command: SoundsCommand) {
        let mut engine = self.engine.lock().await;
        match command {
            SoundsCommand::Pause if !engine.sounds.is_empty() => engine.set_sounds_paused(true),
            SoundsCommand::Pause => {}
            SoundsCommand::Resume => engine.set_sounds_paused(false),
            SoundsCommand::SetVolume(volume) => {
                engine.effects_volume = volume;
                if let Some(output) = &mut engine.output {
                    output.mixer.set_effects_volume(volume.into_inner());
                }
            }
        }
        self.publish(&engine);
    }

    async fn update_sound(&self, entry_id: CollectionEntryId, settings: SoundSettings) {
        let mut engine = self.engine.lock().await;
        let Some(sound) = engine.sounds.get_mut(&entry_id) else {
            return;
        };
        sound.cue.settings = settings;
        if let Some(playback) = &mut sound.playback {
            playback.apply(settings);
        }
        self.publish(&engine);
    }

    async fn select_output(&self, target: OutputTarget) -> Result<(), SelectOutputError> {
        let mut engine = self.engine.lock().await;
        let unchanged = engine.target == target
            && match target {
                OutputTarget::Local => true,
                OutputTarget::DiscordVoice(_) => engine.output.is_some(),
            };
        if unchanged {
            return Ok(());
        }
        let voice_availability = self.outputs.voice_availability();
        if let OutputTarget::DiscordVoice(_) = target {
            match voice_availability {
                VoiceAvailability::NotConfigured => {
                    return Err(SelectOutputError::DiscordNotConfigured);
                }
                VoiceAvailability::Disabled => return Err(SelectOutputError::DiscordDisabled),
                VoiceAvailability::Enabled => {}
            }
        }
        // Ends reconnection attempts to the previous voice channel.
        engine.output_generation += 1;
        if let LocalOutputState::Recovering { previous_failure } = &engine.local_output {
            engine.local_output = previous_failure
                .clone()
                .map_or(LocalOutputState::Released, LocalOutputState::Failed);
        }
        let previous_voice = engine.voice.clone();
        if let OutputTarget::DiscordVoice(_) = target {
            engine.voice = VoiceConnectionState::Connecting;
            self.publish(&engine);
        }
        // songbird keeps one call per guild, so joining another channel of the same guild before
        // leaving would hand the new channel to the old session.
        let leave_first = engine.output.is_some() && shares_guild(engine.target, target);
        if leave_first {
            self.suspend_output(&mut engine).await;
        }
        let needs_output = matches!(target, OutputTarget::DiscordVoice(_)) || engine.has_audio();
        let opened = if needs_output {
            match self
                .outputs
                .open(target, MixerVolumes::with_effects(engine.effects_volume))
                .await
            {
                Ok(opened) => Some(opened),
                Err(error) => {
                    self.recover_from_failed_switch(
                        &mut engine,
                        leave_first,
                        previous_voice,
                        &error,
                    )
                    .await;
                    return Err(error);
                }
            }
        } else {
            None
        };
        self.suspend_output(&mut engine).await;
        engine.target = target;
        engine.local_output = LocalOutputState::Released;
        engine.voice = match target {
            OutputTarget::Local => idle_voice(voice_availability, None),
            OutputTarget::DiscordVoice(_) => VoiceConnectionState::Connected,
        };
        if let Some(opened) = opened {
            self.install_output(&mut engine, opened).await;
        }
        self.publish(&engine);
        Ok(())
    }

    async fn refresh_voice_availability(&self) {
        let mut engine = self.engine.lock().await;
        if engine.target == OutputTarget::Local {
            engine.voice = idle_voice(self.outputs.voice_availability(), None);
            self.publish(&engine);
        }
    }

    async fn stand_by(&self) {
        let mut engine = self.engine.lock().await;
        self.discard_preview(&mut engine).await;
        // Ends reconnection attempts, which would otherwise rejoin a voice channel meanwhile.
        engine.output_generation += 1;
        if let LocalOutputState::Recovering { previous_failure } = &engine.local_output {
            engine.local_output = previous_failure
                .clone()
                .map_or(LocalOutputState::Released, LocalOutputState::Failed);
        }
        engine.stop_music();
        engine.stop_layers();
        let fade = engine.sound_stop_fade(STOP_FADE);
        for sound in std::mem::take(&mut engine.sounds).values_mut() {
            if let Some(playback) = &mut sound.playback {
                playback.stop(fade);
            }
        }
        engine.unpause_when_no_sounds_left();
        engine.browser.retire(BrowserStatus::Closed);
        engine.release_at = Some(Instant::now() + STOP_FADE);
        self.publish(&engine);
    }

    async fn open_browser(&self) -> Result<(), OpenBrowserError> {
        self.open_browser_host().await
    }

    async fn close_browser(&self) {
        self.close_browser_host().await;
    }

    async fn control_browser(&self, command: BrowserCommand) {
        self.control_browser_gain(command).await;
    }

    async fn start_preview(&self, preview: SongPreview) -> Result<PreviewSession, PlayAudioError> {
        self.begin_preview(preview).await
    }

    async fn stop_preview(&self, id: PreviewId) {
        self.end_preview(id).await;
    }

    fn watch_player(&self) -> watch::Receiver<PlayerState> {
        self.state.subscribe()
    }

    fn diagnostics(&self) -> PlayerDiagnostics {
        PlayerDiagnostics {
            music_underrun_frames: self.music_underrun_frames.load(Ordering::Relaxed),
            sound_underrun_frames: self.sound_underrun_frames.load(Ordering::Relaxed),
            local_stream_errors: self.local_stream_errors.load(Ordering::Relaxed),
            local_stream_errors_discarded: self
                .local_stream_errors_discarded
                .load(Ordering::Relaxed),
            browser_underrun_frames: self
                .browser_counters
                .underrun_frames
                .load(Ordering::Relaxed),
            browser_dropped_frames: self.browser_counters.dropped_frames.load(Ordering::Relaxed),
            voice_pacing: self.outputs.voice_pacing(),
        }
    }
}

impl<O: AudioOutputs> EnginePlayer<O> {
    /// Keeps the previous output when it is still open. When it was closed to leave its guild
    /// first, moves to the local output instead, so the music doesn't vanish with the failure.
    async fn recover_from_failed_switch(
        &self,
        engine: &mut Engine,
        previous_closed: bool,
        previous_voice: VoiceConnectionState,
        error: &SelectOutputError,
    ) {
        if matches!(error, SelectOutputError::LocalDevice { .. }) {
            engine.record_local_open_failure(error);
        }
        if !previous_closed {
            engine.voice = previous_voice;
            self.publish(engine);
            return;
        }
        engine.target = OutputTarget::Local;
        engine.voice = idle_voice(self.outputs.voice_availability(), Some(error_chain(error)));
        if engine.has_audio() {
            match self
                .outputs
                .open(
                    OutputTarget::Local,
                    MixerVolumes::with_effects(engine.effects_volume),
                )
                .await
            {
                Ok(opened) => self.install_output(engine, opened).await,
                Err(error) => {
                    engine.record_local_open_failure(&error);
                }
            }
        }
        self.publish(engine);
    }
}

/// Everything the engine plays and where, behind the engine lock.
struct Engine {
    target: OutputTarget,
    local_output: LocalOutputState,
    local_reopens: LocalReopens,
    /// Absent until audio first plays locally, and while a voice connection is down.
    output: Option<OpenedOutput>,
    /// Advances whenever the output changes, so stale reconnection attempts end.
    output_generation: u64,
    voice: VoiceConnectionState,
    music: Option<Music>,
    music_layers: MusicLayers<MusicLayer>,
    /// Advances whenever the current track changes, so superseded changes don't apply.
    music_generation: u64,
    planned_queue: Option<PlannedQueue>,
    volume: Volume,
    effects_volume: Volume,
    muted: bool,
    modes: QueueModes,
    crossfade: CrossfadeDuration,
    sounds: BTreeMap<CollectionEntryId, FiredSound>,
    sounds_paused_at: Option<SystemTime>,
    sound_cache: SoundCache,
    sound_underrun_frames: Arc<AtomicU64>,
    local_stream_errors_retired: LocalStreamErrors,
    last_token: u64,
    retired_underrun_frames: u64,
    /// When the tick closes the output and drops the decoded sounds, if nothing plays by then.
    release_at: Option<Instant>,
    preview: Option<preview::Preview>,
    last_preview_id: u64,
    browser: BrowserSession,
}

impl Engine {
    fn take_output(&mut self) -> Option<OpenedOutput> {
        let mut output = self.output.take()?;
        let errors = output.mixer.poll_local_stream_errors();
        self.local_stream_errors_retired.reported += errors.reported;
        self.local_stream_errors_retired.discarded += errors.discarded;
        Some(output)
    }

    fn poll_local_stream_errors(&mut self) -> LocalStreamErrors {
        let current = self
            .output
            .as_mut()
            .map_or_else(LocalStreamErrors::default, |output| {
                output.mixer.poll_local_stream_errors()
            });
        LocalStreamErrors {
            reported: self.local_stream_errors_retired.reported + current.reported,
            discarded: self.local_stream_errors_retired.discarded + current.discarded,
        }
    }

    fn next_token(&mut self) -> NonZeroU64 {
        self.last_token += 1;
        NonZeroU64::new(self.last_token).unwrap_or(NonZeroU64::MIN)
    }

    fn main_track_loops(&self, loop_region: Option<LoopRegion>) -> bool {
        main_track_loops(self.modes, loop_region)
    }

    fn update_main_track_looping(&self) {
        let Some(music) = &self.music else {
            return;
        };
        if let Some(handle) = &music.current.handle {
            handle.set_looping(self.main_track_loops(music.current.loop_region));
        }
    }

    fn deck(&mut self) -> Option<&mut MusicDeckHandle> {
        self.output.as_mut().map(|output| output.mixer.music())
    }

    fn music_is_playing(&self) -> bool {
        self.music.as_ref().is_some_and(|music| !music.paused) || self.any_layer_playing()
    }

    fn music_is_paused(&self) -> bool {
        (self.music.is_some() || !self.music_layers.is_empty()) && !self.music_is_playing()
    }

    fn has_audio(&self) -> bool {
        self.music.is_some()
            || !self.music_layers.is_empty()
            || !self.sounds.is_empty()
            || self.browser.is_audible()
    }

    /// The fade for a track change someone asked for: the crossfade, clamped between a click-free
    /// cut and [`MANUAL_TRANSITION_MAX`]. With no music playing, or the music paused, the track
    /// starts at once, so the paused track never plays again.
    fn manual_fade(&self) -> Duration {
        if self.music.as_ref().is_none_or(|music| music.paused) {
            return Duration::ZERO;
        }
        self.crossfade
            .into_inner()
            .clamp(ANTI_CLICK_FADE, MANUAL_TRANSITION_MAX)
    }

    fn set_paused(&mut self, paused: bool) -> Result<(), ControlMusicError> {
        if self.music.is_none() && self.music_layers.is_empty() {
            return Err(ControlMusicError::NothingPlaying);
        }
        self.pause_layers(paused)?;
        let Some(music) = &mut self.music else {
            return Ok(());
        };
        if music.paused == paused {
            return Ok(());
        }
        music.paused = paused;
        let Some(deck) = self.deck() else {
            return Ok(());
        };
        let sent = if paused {
            deck.pause(PAUSE_FADE)
        } else {
            deck.resume(PAUSE_FADE)
        };
        sent.map_err(|error| PlayAudioError::OutputUnavailable(error.into()).into())
    }

    fn stop_music(&mut self) {
        self.music_generation += 1;
        let Some(music) = self.music.take() else {
            return;
        };
        if let Some(deck) = self.deck()
            && let Err(error) = deck.stop(STOP_FADE)
        {
            tracing::warn!(%error, "stopping the music failed");
        }
        self.retire_music(&music);
    }

    fn retire_music(&mut self, music: &Music) {
        self.retired_underrun_frames += music.current.underrun_frames()
            + music.armed.as_ref().map_or(0, MusicTrack::underrun_frames);
    }

    /// Drops the armed track, because the queue's following track may have changed.
    fn disarm(&mut self) {
        let Some(music) = &mut self.music else {
            return;
        };
        music.arm_requested = false;
        let Some(armed) = music.armed.take() else {
            return;
        };
        self.retired_underrun_frames += armed.underrun_frames();
        if let Some(deck) = self.deck()
            && let Err(error) = deck.disarm()
        {
            tracing::warn!(%error, "dropping the armed track failed");
        }
    }

    /// The queue a track change in flight installs, else the playing one.
    fn upcoming_queue(&self) -> Option<&MusicQueue> {
        match &self.planned_queue {
            Some(planned) if planned.generation == self.music_generation => Some(&planned.queue),
            _ => self.music.as_ref().map(|music| &music.queue),
        }
    }

    /// Applies `edit` to the queue a track change in flight installs, then to the playing queue,
    /// so the edit survives whether the change applies or fails. Returns the first queue's
    /// result, or `None` without a queue.
    fn edit_queues<E>(
        &mut self,
        mut edit: impl FnMut(&mut MusicQueue) -> Result<(), E>,
    ) -> Option<Result<(), E>> {
        let generation = self.music_generation;
        let planned = self
            .planned_queue
            .as_mut()
            .filter(|planned| planned.generation == generation)
            .map(|planned| &mut planned.queue);
        let playing = self.music.as_mut().map(|music| &mut music.queue);
        let (first, second) = match (planned, playing) {
            (Some(planned), playing) => (planned, playing),
            (None, Some(playing)) => (playing, None),
            (None, None) => return None,
        };
        let edited = edit(first);
        if edited.is_ok()
            && let Some(second) = second
        {
            // The change in flight replaces the playing queue, which may refuse what it accepted.
            let _ = edit(second);
        }
        Some(edited)
    }

    fn following_entry_id(&self) -> Option<CollectionEntryId> {
        let music = self.music.as_ref()?;
        music.queue.following().map(|entry| entry.entry_id)
    }

    /// Drops the armed track when a queue edit changed the track that follows.
    fn disarm_unless_following(&mut self, following: Option<CollectionEntryId>) {
        if self.following_entry_id() != following {
            self.disarm();
        }
    }

    fn apply_music_volume(&mut self) {
        let gain = volume_gain(self.volume, self.muted);
        if let Some(output) = &mut self.output {
            output.mixer.set_music_volume(gain);
        }
    }

    fn set_sounds_paused(&mut self, paused: bool) {
        if self.sounds_paused_at.is_some() == paused {
            return;
        }
        let now = SystemTime::now();
        match self.sounds_paused_at.take() {
            Some(paused_at) => {
                self.delay_sound_starts(now.duration_since(paused_at).unwrap_or_default());
            }
            None => self.sounds_paused_at = Some(now),
        }
        if let Some(output) = &mut self.output {
            if paused {
                output.mixer.pause_sounds(PAUSE_FADE);
            } else {
                output.mixer.resume_sounds(PAUSE_FADE);
            }
        }
    }

    fn delay_sound_starts(&mut self, delay: Duration) {
        for sound in self.sounds.values_mut() {
            sound.started_at += delay;
        }
    }

    fn unpause_when_no_sounds_left(&mut self) {
        if self.sounds.is_empty() {
            self.set_sounds_paused(false);
        }
    }

    /// A paused sound is already silent, and fading it would play the fade on resuming.
    fn sound_stop_fade(&self, fade: Duration) -> Duration {
        if self.sounds_paused_at.is_some() {
            Duration::ZERO
        } else {
            fade
        }
    }

    /// Forgets sounds that finished playing, reporting whether any did.
    fn reap_finished_sounds(&mut self) -> bool {
        let before = self.sounds.len();
        self.sounds.retain(|_, sound| {
            sound
                .playback
                .as_ref()
                .is_none_or(|playback| !playback.is_finished())
        });
        self.sounds.len() != before
    }

    fn underrun_frames(&self) -> u64 {
        self.retired_underrun_frames
            + self.layer_underrun_frames()
            + self.music.as_ref().map_or(0, |music| {
                music.current.underrun_frames()
                    + music.armed.as_ref().map_or(0, MusicTrack::underrun_frames)
            })
    }

    fn snapshot(&self) -> PlayerState {
        let sampled_at = SystemTime::now();
        PlayerState {
            output: self.target,
            local_output: self.local_output.clone(),
            voice: self.voice.clone(),
            music: self.music.as_ref().map(|music| MusicState {
                collection_id: music.queue.current_collection_id(),
                entry: music.queue.current().clone(),
                position: PositionSample {
                    position: music.current.position(),
                    sampled_at,
                },
                duration: music.current.duration,
                paused: music.paused,
                loop_region: music.current.loop_region,
            }),
            queued_entry_ids: self
                .upcoming_queue()
                .map(|queue| queue.queued().map(|song| song.entry.entry_id).collect())
                .unwrap_or_default(),
            music_layers: self.music_layers.map(|layer| layer.snapshot(sampled_at)),
            volume: self.volume,
            effects_volume: self.effects_volume,
            muted: self.muted,
            modes: self.modes,
            crossfade: self.crossfade,
            sounds: self.sounds.values().map(FiredSound::active_sound).collect(),
            sounds_paused: self.sounds_paused_at.is_some(),
            browser: self.browser.playback(),
        }
    }
}

struct MusicChange {
    source: TrackSource,
    start: TrackStart,
}

/// How the new current track starts.
#[derive(Clone, Copy)]
struct TrackStart {
    start_offset: Duration,
    fade_in: Duration,
    fade_out: Duration,
    paused: bool,
    loop_region: Option<LoopRegion>,
    looping: bool,
}

/// The queue a track change leaves behind, from planning until its track opens.
struct PlannedQueue {
    generation: u64,
    queue: MusicQueue,
}

struct MusicPlan {
    generation: u64,
    change: MusicChange,
    sample_rate: u32,
}

struct ArmRequest {
    current_token: NonZeroU64,
    entry_id: CollectionEntryId,
    source: TrackSource,
    sample_rate: u32,
    loop_region: Option<LoopRegion>,
    looping: bool,
}

/// Plays a loaded sound on the engine's output.
async fn fire_sound(
    engine: &mut Engine,
    cue: SoundCue,
    loaded: LoadedSound,
) -> Result<FiredSound, PlayAudioError> {
    let output = engine
        .output
        .as_mut()
        .ok_or_else(|| PlayAudioError::OutputUnavailable(anyhow::anyhow!("no output is open")))?;
    let sample_rate = output.mixer.sample_rate();
    let (duration, source) = match loaded {
        LoadedSound::Decoded(sound) => (sound.duration(), EffectSource::Decoded(sound)),
        LoadedSound::Streamed(file) => {
            let duration = file.duration().unwrap_or_default();
            let options = DecodeOptions {
                start_position: Duration::ZERO,
                looping: cue.settings.looping,
                loop_region: None,
            };
            let mut track = tokio::task::spawn_blocking(move || {
                BufferedTrack::start(file, sample_rate, options)
            })
            .await
            .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?
            .map_err(play_audio_error)?;
            track
                .reader
                .count_underruns_in(Arc::clone(&engine.sound_underrun_frames));
            (duration, EffectSource::Streamed(track))
        }
    };
    let (sound, handle) = effect_sound(source, cue.settings, sample_rate);
    output
        .mixer
        .play_effect(sound)
        .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?;
    Ok(FiredSound {
        cue,
        started_at: SystemTime::now(),
        duration,
        playback: Some(handle),
    })
}

async fn read_sound_key(path: &AudioFilePath) -> Result<SoundKey, PlayAudioError> {
    let path = path.as_ref().to_owned();
    tokio::task::spawn_blocking(move || SoundKey::read(&path))
        .await
        .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => PlayAudioError::NotFound,
            _ => PlayAudioError::Unreadable(error),
        })
}

async fn load_sound_blocking(
    path: &AudioFilePath,
    looping: bool,
) -> Result<LoadedSound, PlayAudioError> {
    let path = path.as_ref().to_owned();
    tokio::task::spawn_blocking(move || {
        load_sound(
            &path,
            SoundLoadOptions {
                looping,
                bytes_max: SOUND_CACHE_BYTES_MAX,
            },
        )
    })
    .await
    .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?
    .map_err(play_audio_error)
}

async fn run_ticks<O: AudioOutputs>(player: Weak<EnginePlayer<O>>) {
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let Some(player) = player.upgrade() else {
            return;
        };
        player.tick().await;
    }
}

/// Repeating the track repeats a saved region; a track without one restarts through the queue.
fn main_track_loops(modes: QueueModes, loop_region: Option<LoopRegion>) -> bool {
    loop_region.is_some() && modes.repeat == RepeatMode::Track
}

fn idle_voice(availability: VoiceAvailability, reason: Option<String>) -> VoiceConnectionState {
    match availability {
        VoiceAvailability::NotConfigured => VoiceConnectionState::NotConfigured,
        VoiceAvailability::Disabled => VoiceConnectionState::Disabled,
        VoiceAvailability::Enabled => VoiceConnectionState::Disconnected { reason },
    }
}

fn shares_guild(from: OutputTarget, to: OutputTarget) -> bool {
    match (from, to) {
        (OutputTarget::DiscordVoice(from), OutputTarget::DiscordVoice(to)) => {
            from.guild_id == to.guild_id
        }
        _ => false,
    }
}

/// A random seed for shuffling, from the standard library's per-process hash keys and the clock.
fn shuffle_seed() -> u64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    hasher.finish()
}

fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
