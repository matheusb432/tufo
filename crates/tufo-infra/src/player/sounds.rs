//! Sound effects the engine plays over the music.

use std::{
    num::NonZeroU64,
    sync::{Arc, PoisonError},
    time::{Duration, Instant, SystemTime},
};

use tufo_application::playback::player::PlayAudioError;
use tufo_models::{
    library::Volume,
    playback::player_state::{ActiveSound, SoundCue},
};

use super::{ANTI_CLICK_FADE, Engine, EnginePlayer, SOUND_CACHE_BYTES_MAX, outputs::AudioOutputs};
use crate::audio::{
    buffered_track::{BufferedTrack, DecodeOptions},
    effect_sound::{EffectSoundHandle, EffectSource, effect_sound},
    memory::{AudioMemory, PreparationCancellation},
    sound_cache::{LoadedSound, SoundKey, SoundLoadOptions, load_sound_with_memory},
};

pub(super) struct SoundPreparation {
    pub(super) token: NonZeroU64,
    pub(super) cancellation: PreparationCancellation,
    pause_generation: u64,
}

pub(super) struct CancelPreparation {
    cancellation: PreparationCancellation,
    armed: bool,
}

impl CancelPreparation {
    pub(super) fn new(cancellation: PreparationCancellation) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    pub(super) fn complete(mut self) {
        self.armed = false;
    }
}

impl Drop for CancelPreparation {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

pub(super) struct PreparedSound {
    duration: Duration,
    source: EffectSource,
}

impl<O: AudioOutputs> EnginePlayer<O> {
    pub(super) async fn prepare_and_play_sound(&self, cue: SoundCue) -> Result<(), PlayAudioError> {
        let permit = Arc::clone(&self.sound_preparations)
            .try_acquire_owned()
            .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?;
        let entry_id = cue.entry_id;
        let (token, cancellation, sample_rate, output_generation) = {
            let mut engine = self.engine.lock().await;
            self.ensure_output(&mut engine).await?;
            engine.cancel_sound_preparation(entry_id);
            let token = engine.next_token();
            let cancellation = PreparationCancellation::default();
            let pause_generation = engine.sounds_pause_generation;
            engine.sound_preparations.insert(
                entry_id,
                SoundPreparation {
                    token,
                    cancellation: cancellation.clone(),
                    pause_generation,
                },
            );
            let sample_rate = engine
                .output
                .as_ref()
                .ok_or_else(|| {
                    PlayAudioError::OutputUnavailable(anyhow::anyhow!("the output closed"))
                })?
                .mixer
                .sample_rate();
            (token, cancellation, sample_rate, engine.output_generation)
        };
        let _cancel_on_drop = CancelPreparation::new(cancellation.clone());
        let player = self.this.upgrade().ok_or_else(|| {
            PlayAudioError::OutputUnavailable(anyhow::anyhow!("the player stopped"))
        })?;
        let worker_cue = cue.clone();
        let worker_cancellation = cancellation.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            // A cancelled RPC cannot release admission while its native file read still runs.
            let _permit = permit;
            let loaded = player.load_sound_for_preparation(&worker_cue, &worker_cancellation)?;
            prepare_sound(
                loaded,
                worker_cue.settings.looping,
                sample_rate,
                &worker_cancellation,
            )
        })
        .await
        .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?;

        let mut engine = self.engine.lock().await;
        if engine
            .sound_preparations
            .get(&entry_id)
            .is_none_or(|request| request.token != token)
        {
            return Ok(());
        }
        let Some(request) = engine.sound_preparations.remove(&entry_id) else {
            return Ok(());
        };
        if cancellation.is_cancelled() || engine.output_generation != output_generation {
            engine.unpause_when_no_sounds_left();
            self.publish(&engine);
            return Ok(());
        }
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                engine.unpause_when_no_sounds_left();
                self.publish(&engine);
                return Err(error);
            }
        };
        if let Some(previous) = engine.sounds.remove(&entry_id)
            && let Some(playback) = previous.playback
        {
            playback.stop(engine.sound_stop_fade(ANTI_CLICK_FADE));
        }
        if request.pause_generation == engine.sounds_pause_generation {
            engine.set_sounds_paused(false);
        }
        let fired = match fire_prepared_sound(&mut engine, cue, prepared) {
            Ok(fired) => fired,
            Err(error) => {
                engine.unpause_when_no_sounds_left();
                self.publish(&engine);
                return Err(error);
            }
        };
        engine.sounds.insert(entry_id, fired);
        self.publish(&engine);
        Ok(())
    }

    fn load_sound_for_preparation(
        &self,
        cue: &SoundCue,
        cancellation: &PreparationCancellation,
    ) -> Result<LoadedSound, PlayAudioError> {
        if cancellation.is_cancelled() {
            return Err(super::play_audio_error(
                crate::audio::buffered_track::OpenTrackError::PreparationCancelled,
            ));
        }
        let key = read_sound_key(cue.path.as_ref())?;
        if let Some(sound) = self
            .engine
            .blocking_lock()
            .sound_cache
            .get(&key, Instant::now())
        {
            return Ok(LoadedSound::Decoded(sound));
        }
        // Cold loads share one gate, so the second request for a file sees the first's cache entry.
        let _load = self
            .sound_loads
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if cancellation.is_cancelled() {
            return Err(super::play_audio_error(
                crate::audio::buffered_track::OpenTrackError::PreparationCancelled,
            ));
        }
        let key = read_sound_key(cue.path.as_ref())?;
        let memory = AudioMemory::shared();
        {
            let mut engine = self.engine.blocking_lock();
            if let Some(sound) = engine.sound_cache.get(&key, Instant::now()) {
                return Ok(LoadedSound::Decoded(sound));
            }
            engine
                .sound_cache
                .evict_unused_for(SOUND_CACHE_BYTES_MAX, &memory);
        }
        let loaded = load_sound_with_memory(
            cue.path.as_ref(),
            SoundLoadOptions {
                looping: cue.settings.looping,
                bytes_max: SOUND_CACHE_BYTES_MAX,
            },
            &memory,
            cancellation,
        )
        .map_err(super::play_audio_error)?;
        if !cancellation.is_cancelled()
            && let LoadedSound::Decoded(sound) = &loaded
        {
            self.engine
                .blocking_lock()
                .sound_cache
                .insert(key, sound.clone(), Instant::now());
        }
        Ok(loaded)
    }
}

fn read_sound_key(path: &std::path::Path) -> Result<SoundKey, PlayAudioError> {
    SoundKey::read(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => PlayAudioError::NotFound,
        _ => PlayAudioError::Unreadable(error),
    })
}

impl Engine {
    pub(super) fn reap_cancelled_sound_preparations(&mut self) -> bool {
        let before = self.sound_preparations.len();
        self.sound_preparations
            .retain(|_, preparation| !preparation.cancellation.is_cancelled());
        let paused_before = self.sounds_paused_at.is_some();
        self.unpause_when_no_sounds_left();
        before != self.sound_preparations.len() || paused_before != self.sounds_paused_at.is_some()
    }

    pub(super) fn cancel_sound_preparation(
        &mut self,
        entry_id: tufo_models::library::CollectionEntryId,
    ) {
        if let Some(request) = self.sound_preparations.remove(&entry_id) {
            request.cancellation.cancel();
        }
    }

    pub(super) fn cancel_sound_preparations(&mut self) {
        for request in std::mem::take(&mut self.sound_preparations).into_values() {
            request.cancellation.cancel();
        }
    }
}

pub(super) fn prepare_sound(
    loaded: LoadedSound,
    looping: bool,
    sample_rate: u32,
    cancellation: &PreparationCancellation,
) -> Result<PreparedSound, PlayAudioError> {
    let (duration, source) = match loaded {
        LoadedSound::Decoded(sound) => (sound.duration(), EffectSource::Decoded(sound)),
        LoadedSound::Streamed(file) => {
            let duration = file.duration().unwrap_or_default();
            let track = BufferedTrack::start_with_memory(
                file,
                sample_rate,
                DecodeOptions {
                    start_position: Duration::ZERO,
                    looping,
                    loop_region: None,
                },
                &AudioMemory::shared(),
                cancellation,
            )
            .map_err(super::play_audio_error)?;
            (duration, EffectSource::Streamed(track))
        }
    };
    Ok(PreparedSound { duration, source })
}

pub(super) fn fire_prepared_sound(
    engine: &mut Engine,
    cue: SoundCue,
    mut prepared: PreparedSound,
) -> Result<FiredSound, PlayAudioError> {
    let output = engine
        .output
        .as_mut()
        .ok_or_else(|| PlayAudioError::OutputUnavailable(anyhow::anyhow!("the output closed")))?;
    if let EffectSource::Streamed(track) = &mut prepared.source {
        track
            .reader
            .count_underruns_in(Arc::clone(&engine.sound_underrun_frames));
    }
    let (sound, handle) = effect_sound(prepared.source, cue.settings, output.mixer.sample_rate());
    output
        .mixer
        .play_effect(sound)
        .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?;
    Ok(FiredSound {
        cue,
        started_at: SystemTime::now(),
        duration: prepared.duration,
        playback: Some(handle),
    })
}

/// A sound the engine fired, kept until it ends. Only looping sounds survive a change of output,
/// with `playback` absent until the new output plays them again.
pub(super) struct FiredSound {
    pub(super) cue: SoundCue,
    pub(super) started_at: SystemTime,
    pub(super) duration: Duration,
    pub(super) playback: Option<EffectSoundHandle>,
}

impl FiredSound {
    pub(super) fn active_sound(&self) -> ActiveSound {
        ActiveSound {
            entry_id: self.cue.entry_id,
            started_at: self.started_at,
            duration: self.duration,
            looping: self.cue.settings.looping,
        }
    }
}

pub(super) fn volume_gain(volume: Volume, muted: bool) -> f32 {
    if muted { 0.0 } else { volume.into_inner() }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Mutex};

    use kira::backend::Renderer;
    use tufo_application::playback::{
        control_sounds::SoundsCommand,
        player::{Player, SongLoops},
        select_output::SelectOutputError,
    };
    use tufo_models::{
        library::{AudioFileId, CollectionEntryId, SoundSettings, song_loop::LoopRegion},
        playback::{AudioFilePath, OutputTarget, diagnostics::VoicePacing},
    };
    use uuid::Uuid;

    use super::*;
    use crate::{
        audio::{Mixer, MixerVolumes},
        browser::UnavailableBrowser,
        discord::VoiceAvailability,
        player::{EngineTimings, outputs::OpenedOutput},
    };

    #[derive(Default)]
    struct PulledOutputs(Mutex<Option<Renderer>>);

    impl AudioOutputs for PulledOutputs {
        fn voice_availability(&self) -> VoiceAvailability {
            VoiceAvailability::Disabled
        }

        fn open(
            &self,
            _: OutputTarget,
            volumes: MixerVolumes,
        ) -> impl std::future::Future<Output = Result<OpenedOutput, SelectOutputError>> + Send
        {
            std::future::ready(open_pulled_output(self, volumes))
        }

        fn close(&self, output: OpenedOutput) -> impl std::future::Future<Output = ()> + Send {
            drop(output);
            self.0.lock().unwrap().take();
            std::future::ready(())
        }

        fn voice_pacing(&self) -> Option<VoicePacing> {
            None
        }
    }

    fn open_pulled_output(
        outputs: &PulledOutputs,
        volumes: MixerVolumes,
    ) -> Result<OpenedOutput, SelectOutputError> {
        let (mixer, receiver) = Mixer::open_pulled(48_000, volumes)
            .map_err(|error| SelectOutputError::VoiceConnection(error.into()))?;
        *outputs.0.lock().unwrap() = Some(receiver.recv().unwrap());
        Ok(OpenedOutput::new(mixer, None))
    }

    struct NoLoops;

    impl SongLoops for NoLoops {
        fn loop_region(&self, _: AudioFileId) -> Option<LoopRegion> {
            None
        }
    }

    fn player_and_rain() -> anyhow::Result<(Arc<EnginePlayer<PulledOutputs>>, SoundCue)> {
        let player = EnginePlayer::with_outputs(
            PulledOutputs::default(),
            EngineTimings::default(),
            NoLoops,
            UnavailableBrowser,
        );
        let cue = SoundCue {
            entry_id: CollectionEntryId::from_uuid(Uuid::new_v4()),
            path: AudioFilePath::try_new(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/effects/rain.ogg"),
            )?,
            settings: SoundSettings {
                looping: true,
                ..SoundSettings::default()
            },
        };
        Ok((player, cue))
    }

    async fn wait_for_preparation(player: &EnginePlayer<PulledOutputs>) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while player.engine.lock().await.sound_preparations.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[expect(
        clippy::await_holding_lock,
        reason = "the test deliberately holds the worker's cold-load gate while exercising control operations"
    )]
    async fn stopping_a_preparing_sound_keeps_controls_responsive_and_prevents_a_late_start()
    -> anyhow::Result<()> {
        let (player, cue) = player_and_rain()?;
        let entry_id = cue.entry_id;
        let load_gate = player.sound_loads.lock().unwrap();
        let playing = Arc::clone(&player);
        let play = tokio::spawn(async move { playing.play_sound(cue).await });
        wait_for_preparation(&player).await;

        tokio::time::timeout(Duration::from_millis(250), async {
            player
                .control_sounds(SoundsCommand::SetVolume(Volume::try_new(0.25).unwrap()))
                .await;
            player.control_sounds(SoundsCommand::Pause).await;
            player.stop_sound(entry_id).await;
        })
        .await?;
        drop(load_gate);
        play.await??;
        assert_eq!(player.watch_player().borrow().sounds, []);
        assert!(!player.watch_player().borrow().sounds_paused);
        assert_eq!(
            player.watch_player().borrow().effects_volume,
            Volume::try_new(0.25)?
        );
        player.shut_down().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[expect(
        clippy::await_holding_lock,
        reason = "the worker is intentionally gated until the later pause command has completed"
    )]
    async fn a_pause_during_preparation_applies_to_the_sound_when_it_starts() -> anyhow::Result<()>
    {
        let (player, cue) = player_and_rain()?;
        let load_gate = player.sound_loads.lock().unwrap();
        let playing = Arc::clone(&player);
        let play = tokio::spawn(async move { playing.play_sound(cue).await });
        wait_for_preparation(&player).await;
        tokio::time::timeout(
            Duration::from_millis(250),
            player.control_sounds(SoundsCommand::Pause),
        )
        .await?;
        drop(load_gate);
        play.await??;
        let state = player.watch_player().borrow().clone();
        assert_eq!(state.sounds.len(), 1);
        assert!(state.sounds_paused);
        let resume_started = SystemTime::now();
        player.control_sounds(SoundsCommand::Resume).await;
        let resumed = player.watch_player().borrow().clone();
        assert!(!resumed.sounds_paused);
        assert!(resumed.sounds[0].started_at >= resume_started);
        assert!(resumed.sounds[0].started_at <= SystemTime::now());
        player.shut_down().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[expect(
        clippy::await_holding_lock,
        reason = "the native worker stays blocked while the test abandons its RPC"
    )]
    async fn an_abandoned_sound_preparation_releases_its_pause_state() -> anyhow::Result<()> {
        let (player, cue) = player_and_rain()?;
        let load_gate = player.sound_loads.lock().unwrap();
        let playing = Arc::clone(&player);
        let play = tokio::spawn(async move { playing.play_sound(cue).await });
        wait_for_preparation(&player).await;
        player.control_sounds(SoundsCommand::Pause).await;
        play.abort();
        assert!(play.await.unwrap_err().is_cancelled());
        player.tick().await;
        assert!(!player.watch_player().borrow().sounds_paused);
        assert!(player.engine.lock().await.sound_preparations.is_empty());
        drop(load_gate);
        player.shut_down().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[expect(
        clippy::await_holding_lock,
        reason = "a warm sound must not wait for an unrelated cold load"
    )]
    async fn a_cached_sound_bypasses_the_cold_load_gate() -> anyhow::Result<()> {
        let (player, cue) = player_and_rain()?;
        player.play_sound(cue.clone()).await?;
        player.stop_sound(cue.entry_id).await;
        let load_gate = player.sound_loads.lock().unwrap();
        tokio::time::timeout(Duration::from_millis(250), player.play_sound(cue)).await??;
        drop(load_gate);
        assert_eq!(player.watch_player().borrow().sounds.len(), 1);
        player.shut_down().await;
        Ok(())
    }
}
