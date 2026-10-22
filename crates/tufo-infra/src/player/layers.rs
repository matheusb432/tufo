//! Extra songs share the main music bus, buffering, and output lifecycle.

use std::time::{Duration, SystemTime};

use tufo_application::playback::{
    add_music_layer::{AddMusicLayerError, MusicLayerCue},
    play_queue::PlayQueueError,
    player::PlayAudioError,
};
use tufo_models::{
    library::{CollectionEntryId, Volume, song_loop::LoopRegion},
    playback::{
        music_layers::{MUSIC_LAYERS_EXTRA_MAX, MUSIC_LAYERS_MAX, MusicLayerState},
        player_state::{MusicState, PositionSample},
        queue::{MusicQueue, QueueStart},
    },
};
use tufo_wire::player::UpdateMusicLayer;

use super::{
    ANTI_CLICK_FADE, Engine, EnginePlayer, MusicTrack, PAUSE_FADE, RESTORE_FADE_IN, STOP_FADE,
    TrackSource, TrackStart, open_track, open_track_cancellable, outputs::AudioOutputs,
    probe_track, shuffle_seed, start_position,
};
use crate::audio::{
    buffered_track::DecodeOptions,
    memory::PreparationCancellation,
    music_deck::{DeckState, DeckTrack, MusicDeckHandle},
};

pub(super) struct MusicLayer {
    cue: MusicLayerCue,
    track: MusicTrack,
    volume: Volume,
    looping: bool,
    paused: bool,
}

impl MusicLayer {
    pub(super) fn entry_id(&self) -> CollectionEntryId {
        self.cue.entry.entry_id
    }

    pub(super) fn snapshot(&self, sampled_at: SystemTime) -> MusicLayerState {
        MusicLayerState {
            music: MusicState {
                collection_id: self.cue.collection_id,
                entry: self.cue.entry.clone(),
                position: PositionSample {
                    position: self.position(),
                    sampled_at,
                },
                duration: self.track.duration,
                paused: self.paused,
                loop_region: self.track.loop_region,
            },
            volume: self.volume,
            looping: self.looping,
        }
    }

    fn position(&self) -> Duration {
        let position = self.track.position();
        match self.track.duration {
            Some(duration)
                if self.looping && self.track.loop_region.is_none() && !duration.is_zero() =>
            {
                Duration::from_secs_f64(position.as_secs_f64() % duration.as_secs_f64())
            }
            _ => position,
        }
    }
}

impl<O: AudioOutputs> EnginePlayer<O> {
    pub(super) async fn add_layer(&self, cue: MusicLayerCue) -> Result<(), AddMusicLayerError> {
        let cancellation = PreparationCancellation::default();
        let _cancel_on_drop = super::sounds::CancelPreparation::new(cancellation.clone());
        let mut engine = self.engine.lock().await;
        if !engine.register_layer_preparation(&cue, cancellation.clone())? {
            return Ok(());
        }
        let entry_id = cue.entry.entry_id;
        drop(engine);
        let file = probe_track(cue.entry.path.clone(), cancellation.clone()).await;
        let mut engine = self.engine.lock().await;
        if cancellation.is_cancelled() {
            return Ok(());
        }
        let file = match file {
            Ok(file) => file,
            Err(error) => {
                engine.layer_preparations.remove(&entry_id);
                return Err(error.into());
            }
        };
        self.ensure_output(&mut engine).await?;
        let sample_rate = engine
            .output
            .as_ref()
            .ok_or_else(|| PlayAudioError::OutputUnavailable(anyhow::anyhow!("no output is open")))?
            .mixer
            .sample_rate();
        let loop_region = self.song_loops.loop_region(cue.entry.audio_file_id);
        let becomes_main_track = engine.music.is_none() && engine.planned_queue.is_none();
        let looping = becomes_main_track && engine.main_track_loops(loop_region);
        let options = DecodeOptions {
            start_position: start_position(loop_region),
            looping,
            loop_region,
        };
        let output_generation = engine.output_generation;
        engine.prepare_track_memory(sample_rate);
        drop(engine);
        let opened = open_track_cancellable(
            TrackSource::Open(file),
            sample_rate,
            options,
            cancellation.clone(),
        )
        .await;
        let mut engine = self.engine.lock().await;
        if cancellation.is_cancelled() {
            return Ok(());
        }
        engine.layer_preparations.remove(&entry_id);
        if engine.output_generation != output_generation {
            return Ok(());
        }
        let opened = opened?;
        if engine.music.is_none() && engine.planned_queue.is_none() {
            let queue = MusicQueue::new(
                QueueStart {
                    collection_id: cue.collection_id,
                    entries: vec![cue.entry],
                    start_index: 0,
                },
                engine.modes,
                shuffle_seed(),
            )
            .map_err(PlayQueueError::from)?;
            let paused = engine.music_paused;
            engine.invalidate_music_preparation();
            Self::install_music_track(
                &mut engine,
                queue,
                opened,
                TrackStart {
                    start_offset: start_position(loop_region),
                    fade_in: ANTI_CLICK_FADE,
                    fade_out: Duration::ZERO,
                    paused,
                    loop_region,
                    looping,
                },
            )?;
        } else {
            engine.install_layer(cue, opened, loop_region)?;
        }
        self.publish(&engine);
        Ok(())
    }

    pub(super) async fn restore_layers(&self, engine: &mut Engine, sample_rate: u32) {
        for slot in 0..MUSIC_LAYERS_EXTRA_MAX {
            if let Err(error) = restore_layer(engine, slot, sample_rate).await {
                tracing::warn!(%error, "a music layer cannot resume on the new output");
                engine.remove_layer(slot);
            }
        }
    }
}

async fn restore_layer(
    engine: &mut Engine,
    slot: usize,
    sample_rate: u32,
) -> Result<(), PlayAudioError> {
    let Some(layer) = engine.music_layers.get(slot) else {
        return Ok(());
    };
    let position = layer.position();
    let paused = layer.paused;
    let volume = layer.volume;
    let duration = layer.track.duration;
    let loop_region = layer.track.loop_region;
    let opened = open_track(
        TrackSource::Open(layer.track.file.clone()),
        sample_rate,
        DecodeOptions {
            start_position: position,
            looping: layer.looping,
            loop_region,
        },
    )
    .await?;
    let token = engine.next_token();
    let output = engine.output.as_mut().ok_or_else(|| {
        PlayAudioError::OutputUnavailable(anyhow::anyhow!("the layer output closed"))
    })?;
    output
        .mixer
        .restore_music_layer_volume(slot, volume.into_inner());
    let deck = output.mixer.music_layer(slot).ok_or_else(|| {
        PlayAudioError::OutputUnavailable(anyhow::anyhow!("the layer deck is absent"))
    })?;
    start_deck(
        deck,
        DeckTrack {
            token,
            reader: opened.track.reader,
        },
        paused,
        RESTORE_FADE_IN,
    )?;
    if let Some(layer) = engine.music_layers.get_mut(slot) {
        layer.track = MusicTrack {
            token,
            file: opened.file,
            start_offset: position,
            duration: opened.duration.or(duration),
            loop_region,
            handle: Some(opened.track.handle),
        };
    }
    Ok(())
}

fn start_deck(
    deck: &mut MusicDeckHandle,
    track: DeckTrack,
    paused: bool,
    fade_in: Duration,
) -> Result<(), PlayAudioError> {
    let pause = if paused {
        deck.pause(Duration::ZERO)
    } else {
        deck.resume(Duration::ZERO)
    };
    pause
        .and_then(|()| deck.start(track, fade_in, Duration::ZERO))
        .map_err(|error| PlayAudioError::OutputUnavailable(error.into()))
}

impl Engine {
    pub(super) fn reconcile_music_pause_intent(&mut self) {
        if let Some(music) = &self.music {
            self.music_paused = music.paused;
        } else if let Some((_, layer)) = self.music_layers.iter().next() {
            self.music_paused = layer.paused;
        } else if !self
            .layer_preparations
            .values()
            .any(|request| !request.is_cancelled())
        {
            self.music_paused = false;
        }
    }

    fn register_layer_preparation(
        &mut self,
        cue: &MusicLayerCue,
        cancellation: PreparationCancellation,
    ) -> Result<bool, AddMusicLayerError> {
        self.reap_cancelled_music_preparation();
        self.layer_preparations
            .retain(|_, request| !request.is_cancelled());
        if self.music.is_none()
            && self.music_layers.is_empty()
            && self.planned_queue.is_none()
            && self.layer_preparations.is_empty()
        {
            self.music_paused = false;
        }
        let entry_id = cue.entry.entry_id;
        if self
            .upcoming_queue()
            .is_some_and(|queue| queue.current().entry_id == entry_id)
            || self
                .music_layers
                .iter()
                .any(|(_, layer)| layer.entry_id() == entry_id)
            || self.layer_preparations.contains_key(&entry_id)
        {
            return Ok(false);
        }
        let main_reserved = self.music.is_some() || self.planned_queue.is_some();
        if usize::from(main_reserved) + self.music_layers.len() + self.layer_preparations.len()
            >= MUSIC_LAYERS_MAX
        {
            return Err(AddMusicLayerError::Full);
        }
        self.layer_preparations.insert(entry_id, cancellation);
        Ok(true)
    }

    fn layer_deck(
        &mut self,
        slot: usize,
    ) -> Option<&mut crate::audio::music_deck::MusicDeckHandle> {
        self.output.as_mut()?.mixer.music_layer(slot)
    }

    pub(super) fn stop_layer(&mut self, entry_id: CollectionEntryId) {
        if let Some(preparation) = self.layer_preparations.remove(&entry_id) {
            preparation.cancel();
        }
        let slot = self
            .music_layers
            .iter()
            .find(|(_, layer)| layer.entry_id() == entry_id)
            .map(|(slot, _)| slot);
        if let Some(slot) = slot {
            self.remove_layer(slot);
        }
    }

    fn remove_layer(&mut self, slot: usize) {
        let Some(layer) = self.music_layers.remove(slot) else {
            return;
        };
        self.retired_underrun_frames += layer.track.underrun_frames();
        let Some(deck) = self.layer_deck(slot) else {
            return;
        };
        let fade = if layer.paused {
            Duration::ZERO
        } else {
            STOP_FADE
        };
        if let Err(error) = deck.stop(fade) {
            tracing::warn!(%error, "stopping a music layer failed");
        }
    }

    fn install_layer(
        &mut self,
        cue: MusicLayerCue,
        opened: super::OpenedTrack,
        loop_region: Option<LoopRegion>,
    ) -> Result<(), AddMusicLayerError> {
        let paused = self.music_paused;
        opened.track.handle.set_looping(false);
        let token = self.next_token();
        let track = MusicTrack {
            token,
            file: opened.file,
            start_offset: start_position(loop_region),
            duration: opened.duration.or(cue.entry.duration),
            loop_region,
            handle: Some(opened.track.handle),
        };
        let slot = self
            .music_layers
            .try_insert(MusicLayer {
                cue,
                track,
                volume: Volume::default(),
                looping: false,
                paused,
            })
            .map_err(|_| AddMusicLayerError::Full)?;
        if let Some(output) = &mut self.output {
            output.mixer.set_music_layer_volume(slot, 1.0);
        }
        let started = self
            .layer_deck(slot)
            .ok_or_else(|| {
                PlayAudioError::OutputUnavailable(anyhow::anyhow!("the layer output closed"))
            })
            .and_then(|deck| {
                start_deck(
                    deck,
                    DeckTrack {
                        token,
                        reader: opened.track.reader,
                    },
                    paused,
                    ANTI_CLICK_FADE,
                )
            });
        if let Err(error) = started {
            self.music_layers.remove(slot);
            return Err(error.into());
        }
        Ok(())
    }

    pub(super) fn stop_layers(&mut self) {
        self.cancel_layer_preparations();
        for slot in 0..MUSIC_LAYERS_EXTRA_MAX {
            self.remove_layer(slot);
        }
    }

    pub(super) fn cancel_layer_preparations(&mut self) {
        for preparation in std::mem::take(&mut self.layer_preparations).into_values() {
            preparation.cancel();
        }
    }

    pub(super) fn pause_layers(&mut self, paused: bool) -> Result<(), PlayAudioError> {
        for (_, layer) in self.music_layers.iter_mut() {
            layer.paused = paused;
        }
        let Some(output) = &mut self.output else {
            return Ok(());
        };
        for (slot, _) in self.music_layers.iter() {
            let Some(deck) = output.mixer.music_layer(slot) else {
                continue;
            };
            let sent = if paused {
                deck.pause(PAUSE_FADE)
            } else {
                deck.resume(PAUSE_FADE)
            };
            sent.map_err(|error| PlayAudioError::OutputUnavailable(error.into()))?;
        }
        Ok(())
    }

    pub(super) fn update_layer(&mut self, entry_id: CollectionEntryId, update: UpdateMusicLayer) {
        let Some((slot, layer)) = self
            .music_layers
            .iter_mut()
            .find(|(_, layer)| layer.entry_id() == entry_id)
        else {
            return;
        };
        if let Some(volume) = update.volume {
            layer.volume = volume;
        }
        if let Some(looping) = update.looping {
            layer.looping = looping;
            if let Some(handle) = &layer.track.handle {
                handle.set_looping(looping);
            }
        }
        let volume = layer.volume;
        if let Some(output) = &mut self.output {
            output
                .mixer
                .set_music_layer_volume(slot, volume.into_inner());
        }
    }

    pub(super) fn suspend_layers(&mut self) {
        self.cancel_layer_preparations();
        let mut retired = 0;
        for (_, layer) in self.music_layers.iter_mut() {
            layer.track.start_offset = layer.position();
            retired += layer.track.underrun_frames();
            layer.track.handle = None;
        }
        self.retired_underrun_frames += retired;
    }

    pub(super) fn reap_finished_layers(&mut self) -> bool {
        let mut changed = false;
        for slot in 0..MUSIC_LAYERS_EXTRA_MAX {
            if self.music_layers.get(slot).is_none() {
                continue;
            }
            let Some(deck) = self.layer_deck(slot) else {
                continue;
            };
            deck.drop_retired();
            if deck.state() != DeckState::Idle {
                continue;
            }
            self.remove_layer(slot);
            changed = true;
        }
        changed
    }

    pub(super) fn any_layer_playing(&self) -> bool {
        self.music_layers.iter().any(|(_, layer)| !layer.paused)
    }

    pub(super) fn layer_underrun_frames(&self) -> u64 {
        self.music_layers
            .iter()
            .map(|(_, layer)| layer.track.underrun_frames())
            .sum()
    }
}
