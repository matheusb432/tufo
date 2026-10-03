//! Drives the engine through pulled outputs that a thread renders in real time, like a sound
//! card, recording everything each output played.

use std::{
    collections::HashMap,
    future::Future,
    num::{NonZeroU32, NonZeroU64},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::Context as _;
use kira::backend::Renderer;
use tokio::sync::{oneshot, watch};
use tufo_application::{
    library::{Library, import_paths, update_collection_entry},
    playback::{
        add_music_layer::{AddMusicLayerError, MusicLayerCue},
        add_to_queue::{self, QueueSongError},
        control_music::{ControlMusicError, MusicCommand},
        control_sounds::{self, SoundsCommand},
        play_entry_sound,
        player::{Player, SongLoops, SongPreview},
        resume_saved_loops::{self, ResumeSavedLoopsOutcome},
        select_output::SelectOutputError,
        stop_sound,
    },
};
use tufo_infra::{
    audio::Mixer,
    database::open_database,
    discord::VoiceAvailability,
    library_files::DiskLibraryFiles,
    player::{
        EnginePlayer, EngineTimings, ReconnectPolicy,
        outputs::{AudioOutputs, OpenedOutput},
    },
};
use tufo_models::{
    discord::{GuildId, VoiceChannelId, VoiceChannelTarget},
    library::{
        AudioFileId, CollectionEntryId, CollectionId, CollectionKind, SoundSettings, Volume,
        song_loop::{LoopMarkers, LoopRegion, SeamPreviewSide, SourceFrames, SourceTiming},
    },
    path::LocalPath,
    playback::{
        AudioFilePath, OutputTarget,
        diagnostics::VoicePacing,
        player_state::{
            ActiveSound, CrossfadeDuration, PlayerState, SoundCue, VoiceConnectionState,
        },
        queue::{QueueEntry, QueueStart, QueuedEntry, RepeatMode},
    },
};
use tufo_wire::{
    library::{ImportPaths, UpdateCollectionEntry},
    song_loops::PreviewMode,
};
use uuid::Uuid;

mod support;

use support::{SAMPLE_RATE, constant, ramp, ramp_position_seconds, write_wav};

const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const LOCAL_SAMPLE_RATE: u32 = 44_100;
const SOUND_CARD_PERIOD: Duration = Duration::from_millis(10);
/// Music and sounds fade in after an output change, the music for 50 ms and sounds for 100 ms, so
/// levels are read 100 ms into the new output's recording.
const AFTER_FADE_IN_FRAMES: usize = 4_800;
const AFTER_FADE_IN_SECONDS: f32 = 0.1;
const THREE_SECONDS: usize = 3 * SAMPLE_RATE as usize;
/// The local output's 100 ms fade when the sounds pause or resume, plus one sound card period.
const SOUNDS_PAUSE_FADE_FRAMES: usize = 4_410 + 441;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// What the test controls about its outputs.
#[derive(Default)]
struct OutputControls {
    voice_lost: Mutex<Vec<oneshot::Sender<String>>>,
    fail_voice_opens: AtomicBool,
    voice_disabled: AtomicBool,
    closes: AtomicUsize,
}

impl OutputControls {
    fn drop_voice_connection(&self) -> anyhow::Result<()> {
        let sender = self
            .voice_lost
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop()
            .context("no voice connection is open")?;
        sender
            .send("the test dropped it".to_owned())
            .map_err(|_| anyhow::anyhow!("the engine stopped watching the connection"))
    }
}

struct TestOutputs {
    controls: Arc<OutputControls>,
    renderers: Mutex<mpsc::Sender<(u32, Renderer)>>,
}

impl TestOutputs {
    fn open_pulled(
        &self,
        target: OutputTarget,
        effects_volume: Volume,
    ) -> anyhow::Result<OpenedOutput> {
        let sample_rate = match target {
            OutputTarget::Local => LOCAL_SAMPLE_RATE,
            OutputTarget::DiscordVoice(_) => SAMPLE_RATE,
        };
        let (mixer, renderer_receiver) = Mixer::open_pulled(sample_rate, effects_volume)?;
        self.renderers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send((sample_rate, renderer_receiver.recv()?))
            .map_err(|_| anyhow::anyhow!("the sound card stopped"))?;
        let voice_lost = match target {
            OutputTarget::Local => None,
            OutputTarget::DiscordVoice(_) => {
                let (sender, receiver) = oneshot::channel();
                self.controls
                    .voice_lost
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(sender);
                Some(receiver)
            }
        };
        Ok(OpenedOutput::new(mixer, voice_lost))
    }
}

impl AudioOutputs for TestOutputs {
    fn voice_availability(&self) -> VoiceAvailability {
        if self.controls.voice_disabled.load(Ordering::Relaxed) {
            VoiceAvailability::Disabled
        } else {
            VoiceAvailability::Enabled
        }
    }

    fn open(
        &self,
        target: OutputTarget,
        effects_volume: Volume,
    ) -> impl Future<Output = Result<OpenedOutput, SelectOutputError>> + Send {
        let refused = matches!(target, OutputTarget::DiscordVoice(_))
            && self.controls.fail_voice_opens.load(Ordering::Relaxed);
        let opened = if refused {
            Err(anyhow::anyhow!("the test refused the connection"))
        } else {
            self.open_pulled(target, effects_volume)
        };
        std::future::ready(opened.map_err(SelectOutputError::VoiceConnection))
    }

    fn close(&self, output: OpenedOutput) -> impl Future<Output = ()> + Send {
        self.controls.closes.fetch_add(1, Ordering::Relaxed);
        drop(output);
        std::future::ready(())
    }

    fn voice_pacing(&self) -> Option<VoicePacing> {
        None
    }
}

type Captures = Arc<Mutex<Vec<Vec<f32>>>>;

/// Pulls the newest output's renderer every [`SOUND_CARD_PERIOD`] and records the left channel of
/// each output separately.
struct SoundCard {
    captures: Captures,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl SoundCard {
    fn start(renderers: mpsc::Receiver<(u32, Renderer)>) -> Self {
        let captures = Captures::default();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::spawn({
            let captures = Arc::clone(&captures);
            let stop = Arc::clone(&stop);
            move || run_sound_card(&renderers, &captures, &stop)
        });
        Self {
            captures,
            stop,
            thread: Some(thread),
        }
    }

    fn captures(&self) -> Vec<Vec<f32>> {
        self.captures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for SoundCard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_sound_card(
    renderers: &mpsc::Receiver<(u32, Renderer)>,
    captures: &Captures,
    stop: &AtomicBool,
) {
    let mut current = None;
    while !stop.load(Ordering::Relaxed) {
        if let Some(next) = renderers.try_iter().last() {
            current = Some(next);
            lock(captures).push(Vec::new());
        }
        if let Some((sample_rate, renderer)) = &mut current {
            let frames = *sample_rate as usize / 100;
            record(captures, support::render(renderer, frames));
        }
        thread::sleep(SOUND_CARD_PERIOD);
    }
}

fn record(captures: &Captures, samples: Vec<f32>) {
    if let Some(capture) = lock(captures).last_mut() {
        capture.extend(samples);
    }
}

fn lock(captures: &Captures) -> std::sync::MutexGuard<'_, Vec<Vec<f32>>> {
    captures.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Default)]
struct TestSongLoops(Arc<Mutex<HashMap<AudioFileId, LoopRegion>>>);

impl SongLoops for TestSongLoops {
    fn loop_region(&self, audio_file_id: AudioFileId) -> Option<LoopRegion> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&audio_file_id)
            .copied()
    }
}

struct Rig {
    player: Arc<EnginePlayer<TestOutputs>>,
    controls: Arc<OutputControls>,
    song_loops: TestSongLoops,
    sound_card: SoundCard,
    states: watch::Receiver<PlayerState>,
    directory: tempfile::TempDir,
}

impl Rig {
    fn start() -> anyhow::Result<Self> {
        Self::with_timings(EngineTimings {
            reconnect_policy: ReconnectPolicy {
                attempts_max: 3,
                delay_min: Duration::from_millis(20),
                delay_max: Duration::from_millis(40),
            },
            ..EngineTimings::default()
        })
    }

    fn with_timings(timings: EngineTimings) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let controls = Arc::new(OutputControls::default());
        let song_loops = TestSongLoops::default();
        let player = EnginePlayer::with_outputs(
            TestOutputs {
                controls: Arc::clone(&controls),
                renderers: Mutex::new(sender),
            },
            timings,
            song_loops.clone(),
        );
        let states = player.watch_player();
        Ok(Self {
            player,
            controls,
            song_loops,
            sound_card: SoundCard::start(receiver),
            states,
            directory: tempfile::tempdir()?,
        })
    }

    fn save_loop(&self, entry: &QueueEntry, region: LoopRegion) {
        self.song_loops
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(entry.audio_file_id, region);
    }

    fn file(&self, name: &str, samples: &[f32]) -> anyhow::Result<AudioFilePath> {
        let path = self.directory.path().join(name);
        write_wav(&path, SAMPLE_RATE, samples)?;
        Ok(AudioFilePath::try_new(path)?)
    }

    async fn wait_for(
        &mut self,
        condition: impl Fn(&PlayerState) -> bool,
    ) -> anyhow::Result<PlayerState> {
        let state =
            tokio::time::timeout(WAIT_TIMEOUT, self.states.wait_for(|state| condition(state)))
                .await
                .context("the player never reached the expected state")??;
        Ok(state.clone())
    }

    fn recorded_frames(&self, output: usize) -> usize {
        self.sound_card.captures().get(output).map_or(0, Vec::len)
    }

    async fn wait_for_recorded_frames(&self, output: usize, frames: usize) -> anyhow::Result<()> {
        tokio::time::timeout(WAIT_TIMEOUT, async {
            while self.recorded_frames(output) < frames {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .with_context(|| format!("output {output} never recorded {frames} frames"))
    }

    fn state(&self) -> PlayerState {
        self.states.borrow().clone()
    }

    fn closes(&self) -> usize {
        self.controls.closes.load(Ordering::Relaxed)
    }

    async fn wait_for_closes(&self, closes: usize) -> anyhow::Result<()> {
        tokio::time::timeout(WAIT_TIMEOUT, async {
            while self.closes() < closes {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .context("the engine never closed the output")
    }

    async fn play(&self, entries: Vec<QueueEntry>) -> anyhow::Result<()> {
        let start = QueueStart {
            collection_id: CollectionId::from_uuid(Uuid::now_v7()),
            entries,
            start_index: 0,
        };
        Ok(self.player.play_queue(start).await?)
    }

    /// Plays one track once, so repeating the playlist doesn't crossfade it into its own start.
    async fn play_once(&self, entry: QueueEntry) -> anyhow::Result<()> {
        self.control(MusicCommand::SetRepeat(RepeatMode::Off))
            .await?;
        self.play(vec![entry]).await
    }

    async fn control(&self, command: MusicCommand) -> anyhow::Result<()> {
        Ok(self.player.control_music(command).await?)
    }
}

fn sound(path: AudioFilePath, looping: bool) -> SoundCue {
    SoundCue {
        entry_id: CollectionEntryId::from_uuid(Uuid::now_v7()),
        path,
        settings: SoundSettings {
            looping,
            ..SoundSettings::default()
        },
    }
}

fn entry(path: AudioFilePath, title: &str) -> QueueEntry {
    QueueEntry {
        entry_id: CollectionEntryId::from_uuid(Uuid::now_v7()),
        audio_file_id: AudioFileId::from_uuid(Uuid::now_v7()),
        path,
        title: title.to_owned(),
        duration: None,
    }
}

/// A song from another playlist than the one playing.
fn queued(path: AudioFilePath, title: &str) -> QueuedEntry {
    QueuedEntry {
        collection_id: CollectionId::from_uuid(Uuid::now_v7()),
        entry: entry(path, title),
    }
}

fn voice_channel() -> OutputTarget {
    OutputTarget::DiscordVoice(VoiceChannelTarget {
        guild_id: GuildId::new(NonZeroU64::MIN),
        channel_id: VoiceChannelId::new(NonZeroU64::MIN),
    })
}

fn layer(path: AudioFilePath, title: &str) -> MusicLayerCue {
    MusicLayerCue {
        collection_id: CollectionId::from_uuid(Uuid::now_v7()),
        entry: entry(path, title),
    }
}

async fn recorded_level(rig: &Rig, output: usize) -> anyhow::Result<f32> {
    let end = rig.recorded_frames(output) + 20_000;
    rig.wait_for_recorded_frames(output, end).await?;
    let captures = rig.sound_card.captures();
    let capture = &captures[output];
    Ok(capture[capture.len() - 100..].iter().sum::<f32>() / 100.0)
}

#[tokio::test]
async fn effects_volume_multiplies_decoded_and_streamed_sounds_independently_of_music()
-> anyhow::Result<()> {
    let rig = Rig::start()?;
    let main = rig.file("music.wav", &constant(0.1, SAMPLE_RATE as usize * 12))?;
    let mut decoded = sound(
        rig.file("decoded.wav", &constant(0.5, SAMPLE_RATE as usize * 2))?,
        true,
    );
    decoded.settings.volume = Volume::try_new(0.4)?;
    let mut streamed = sound(
        rig.file("streamed.wav", &constant(0.25, SAMPLE_RATE as usize * 21))?,
        false,
    );
    streamed.settings.volume = Volume::try_new(0.8)?;
    rig.player
        .control_sounds(SoundsCommand::SetVolume(Volume::try_new(0.5)?))
        .await;
    rig.play_once(entry(main, "Music")).await?;
    rig.player.play_sound(decoded.clone()).await?;
    rig.player.play_sound(streamed).await?;
    assert!((recorded_level(&rig, 0).await? - 0.3).abs() < 0.002);

    rig.player
        .control_sounds(SoundsCommand::SetVolume(Volume::try_new(0.25)?))
        .await;
    assert!((recorded_level(&rig, 0).await? - 0.2).abs() < 0.002);
    decoded.settings.volume = Volume::try_new(0.8)?;
    rig.player
        .update_sound(decoded.entry_id, decoded.settings)
        .await;
    assert!((recorded_level(&rig, 0).await? - 0.25).abs() < 0.002);

    rig.control(MusicCommand::SetVolume(Volume::try_new(0.5)?))
        .await?;
    assert!((recorded_level(&rig, 0).await? - 0.2).abs() < 0.002);
    rig.control(MusicCommand::SetMuted(true)).await?;
    assert!((recorded_level(&rig, 0).await? - 0.15).abs() < 0.002);
    rig.player
        .control_sounds(SoundsCommand::SetVolume(Volume::try_new(0.0)?))
        .await;
    assert!(recorded_level(&rig, 0).await?.abs() < 0.002);
    rig.control(MusicCommand::SetMuted(false)).await?;
    assert!((recorded_level(&rig, 0).await? - 0.05).abs() < 0.002);

    rig.player
        .control_sounds(SoundsCommand::SetVolume(Volume::default()))
        .await;
    assert!((recorded_level(&rig, 0).await? - 0.65).abs() < 0.002);
    assert_eq!(rig.state().effects_volume, Volume::default());
    Ok(())
}

#[tokio::test]
async fn muted_effects_are_silent_from_the_first_frame_of_a_new_output() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    rig.player
        .control_sounds(SoundsCommand::SetVolume(Volume::try_new(0.0)?))
        .await;
    let mut rain = sound(
        rig.file("rain.wav", &constant(0.5, SAMPLE_RATE as usize * 2))?,
        true,
    );
    rain.settings.fade_in = tufo_models::library::FadeDuration::try_new(Duration::ZERO)?;
    rig.player.play_sound(rain).await?;
    rig.wait_for_recorded_frames(0, 11_025).await?;
    let peak = rig.sound_card.captures()[0]
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0_f32, f32::max);
    assert!(peak < 0.000_001, "muted output peaked at {peak}");
    rig.player.select_output(voice_channel()).await?;
    rig.wait_for_recorded_frames(1, 12_000).await?;
    assert!(within(&rig.sound_card.captures()[1], 0.0, 0.000_001));
    Ok(())
}

#[tokio::test]
async fn effects_volume_survives_output_changes_pause_and_standby() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let mut rain = sound(
        rig.file("rain.wav", &constant(0.5, SAMPLE_RATE as usize * 2))?,
        true,
    );
    rain.settings.volume = Volume::try_new(0.4)?;
    rig.player.play_sound(rain.clone()).await?;
    rig.player
        .control_sounds(SoundsCommand::SetVolume(Volume::try_new(0.5)?))
        .await;
    assert!((recorded_level(&rig, 0).await? - 0.1).abs() < 0.002);
    rig.player.control_sounds(SoundsCommand::Pause).await;
    rig.player.select_output(voice_channel()).await?;
    assert!(recorded_level(&rig, 1).await?.abs() < 0.002);
    rig.player
        .control_sounds(SoundsCommand::SetVolume(Volume::try_new(0.25)?))
        .await;
    assert!(rig.state().sounds_paused);
    rig.player.control_sounds(SoundsCommand::Resume).await;
    assert!((recorded_level(&rig, 1).await? - 0.05).abs() < 0.002);

    let closes = rig.closes();
    rig.player.stand_by().await;
    rig.wait_for_closes(closes + 1).await?;
    rig.player.play_sound(rain).await?;
    assert!((recorded_level(&rig, 2).await? - 0.05).abs() < 0.002);
    assert_eq!(rig.state().effects_volume, Volume::try_new(0.25)?);
    Ok(())
}

#[tokio::test]
async fn music_layers_mix_with_independent_gains_and_share_pause_mute_and_volume()
-> anyhow::Result<()> {
    let rig = Rig::start()?;
    let main = rig.file("main.wav", &constant(0.1, SAMPLE_RATE as usize * 8))?;
    let rain = layer(
        rig.file("rain.wav", &constant(0.2, SAMPLE_RATE as usize * 8))?,
        "Rain",
    );
    let wind = layer(
        rig.file("wind.wav", &constant(0.3, SAMPLE_RATE as usize * 8))?,
        "Wind",
    );
    rig.play_once(entry(main, "Main")).await?;
    rig.player.add_music_layer(rain.clone()).await?;
    rig.player.add_music_layer(wind.clone()).await?;
    assert!((recorded_level(&rig, 0).await? - 0.6).abs() < 0.002);

    rig.player
        .update_music_layer(
            rain.entry.entry_id,
            tufo_wire::player::UpdateMusicLayer {
                volume: Some(tufo_models::library::Volume::try_new(0.5)?),
                looping: Some(true),
            },
        )
        .await;
    rig.control(MusicCommand::SetVolume(
        tufo_models::library::Volume::try_new(0.5)?,
    ))
    .await?;
    assert!((recorded_level(&rig, 0).await? - 0.25).abs() < 0.002);

    rig.control(MusicCommand::Pause).await?;
    assert!(recorded_level(&rig, 0).await?.abs() < 0.002);
    assert!(
        rig.state()
            .music_layers
            .iter()
            .all(|(_, layer)| layer.music.paused)
    );
    rig.control(MusicCommand::Resume).await?;
    assert!((recorded_level(&rig, 0).await? - 0.25).abs() < 0.002);
    rig.control(MusicCommand::SetMuted(true)).await?;
    assert!(recorded_level(&rig, 0).await?.abs() < 0.002);
    rig.control(MusicCommand::SetMuted(false)).await?;

    rig.player.stop_music_layer(rain.entry.entry_id).await;
    assert!((recorded_level(&rig, 0).await? - 0.2).abs() < 0.002);
    rig.player.add_music_layer(rain).await?;
    assert!((recorded_level(&rig, 0).await? - 0.3).abs() < 0.002);
    rig.control(MusicCommand::Stop).await?;
    assert!(rig.state().music.is_none() && rig.state().music_layers.is_empty());
    Ok(())
}

#[tokio::test]
async fn concurrent_layer_additions_allow_three_songs_and_duplicates_do_not_restart()
-> anyhow::Result<()> {
    let rig = Rig::start()?;
    let a = layer(rig.file("a.wav", &constant(0.1, THREE_SECONDS))?, "A");
    let b = layer(rig.file("b.wav", &constant(0.1, THREE_SECONDS))?, "B");
    let c = layer(rig.file("c.wav", &constant(0.1, THREE_SECONDS))?, "C");
    let d = layer(rig.file("d.wav", &constant(0.1, THREE_SECONDS))?, "D");
    let (a_result, b_result, c_result, d_result) = tokio::join!(
        rig.player.add_music_layer(a.clone()),
        rig.player.add_music_layer(b.clone()),
        rig.player.add_music_layer(c.clone()),
        rig.player.add_music_layer(d.clone())
    );
    let results = [a_result, b_result, c_result, d_result];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 3);
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| matches!(error, AddMusicLayerError::Full))
    );
    assert_eq!(rig.state().music_layers.len(), 2);
    assert!((recorded_level(&rig, 0).await? - 0.3).abs() < 0.002);
    let active = rig
        .state()
        .music_layers
        .iter()
        .next()
        .map(|(_, layer)| layer.music.entry.entry_id)
        .unwrap();
    let cue = [a, b, c, d]
        .into_iter()
        .find(|cue| cue.entry.entry_id == active)
        .unwrap();
    let before = rig.state().music_layers.clone();
    rig.player.add_music_layer(cue).await?;
    assert_eq!(rig.state().music_layers, before);
    Ok(())
}

#[tokio::test]
async fn output_switching_restores_all_music_layers_at_their_positions_while_paused()
-> anyhow::Result<()> {
    let rig = Rig::start()?;
    rig.play_once(entry(
        rig.file("main.wav", &constant(0.1, THREE_SECONDS))?,
        "Main",
    ))
    .await?;
    let rain = layer(rig.file("rain.wav", &ramp(THREE_SECONDS))?, "Rain");
    rig.player.add_music_layer(rain.clone()).await?;
    rig.player
        .update_music_layer(
            rain.entry.entry_id,
            tufo_wire::player::UpdateMusicLayer {
                volume: None,
                looping: Some(true),
            },
        )
        .await;
    rig.wait_for_recorded_frames(0, LOCAL_SAMPLE_RATE as usize / 5)
        .await?;
    rig.control(MusicCommand::Pause).await?;
    rig.wait_for_recorded_frames(0, rig.recorded_frames(0) + LOCAL_SAMPLE_RATE as usize / 5)
        .await?;
    rig.player.select_output(voice_channel()).await?;
    let paused = rig.state();
    let layer = &paused.music_layers.iter().next().unwrap().1.music;
    let paused_position = layer.position.position;
    assert!(layer.paused);
    assert!(recorded_level(&rig, 1).await?.abs() < 0.002);
    rig.control(MusicCommand::Resume).await?;
    rig.wait_for_recorded_frames(1, rig.recorded_frames(1) + 9_600)
        .await?;
    let captures = rig.sound_card.captures();
    let expected = paused_position.as_secs_f32();
    let samples = from_first_sound(&captures[1]);
    let resumed =
        ramp_position_seconds(samples[AFTER_FADE_IN_FRAMES] - 0.1) - AFTER_FADE_IN_SECONDS;
    assert!(
        (resumed - expected).abs() < 0.03,
        "resumed at {resumed}, expected near {expected}"
    );
    assert_eq!(rig.state().music_layers.len(), 1);
    assert!(
        rig.state()
            .music_layers
            .iter()
            .all(|(_, layer)| layer.looping && !layer.music.paused)
    );
    rig.player.stand_by().await;
    assert!(rig.state().music_layers.is_empty());
    Ok(())
}

#[tokio::test]
async fn an_extra_song_keeps_playing_after_the_main_track_stops_and_frees_its_slot_at_the_end()
-> anyhow::Result<()> {
    let mut rig = Rig::start()?;
    let main = entry(rig.file("main.wav", &constant(0.1, 4_800))?, "Main");
    let extra = layer(rig.file("extra.wav", &constant(0.2, 24_000))?, "Extra");
    rig.play_once(main).await?;
    rig.player.add_music_layer(extra).await?;
    rig.wait_for(|state| state.music.is_none() && state.music_layers.len() == 1)
        .await?;
    rig.control(MusicCommand::Pause).await?;
    assert!(
        rig.state()
            .music_layers
            .iter()
            .all(|(_, layer)| layer.music.paused)
    );
    rig.control(MusicCommand::Resume).await?;
    rig.wait_for(|state| state.music.is_none() && state.music_layers.is_empty())
        .await?;
    Ok(())
}

fn playing_title(state: &PlayerState) -> Option<&str> {
    state.music.as_ref().map(|music| music.entry.title.as_str())
}

/// The position the state sampled. The test sound card runs slower than the wall clock, so
/// advancing the sample by wall time would overshoot what it played.
fn position_seconds(state: &PlayerState) -> anyhow::Result<f32> {
    let music = state.music.as_ref().context("no music is playing")?;
    Ok(music.position.position.as_secs_f32())
}

/// The recording from its first sound on.
fn from_first_sound(capture: &[f32]) -> &[f32] {
    let start = capture
        .iter()
        .position(|sample| *sample != 0.0)
        .unwrap_or(capture.len());
    &capture[start..]
}

/// The ramp position an output played once its fade-in was over, minus the fade-in.
fn resumed_ramp_position(capture: &[f32]) -> anyhow::Result<f32> {
    let sample = from_first_sound(capture)
        .get(AFTER_FADE_IN_FRAMES)
        .context("the output played too little")?;
    Ok(ramp_position_seconds(*sample) - AFTER_FADE_IN_SECONDS)
}

fn within(samples: &[f32], value: f32, tolerance: f32) -> bool {
    samples
        .iter()
        .all(|sample| (sample - value).abs() < tolerance)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queue_plays_its_tracks_back_to_back_and_stops_after_the_last() -> anyhow::Result<()> {
    let mut rig = Rig::start()?;
    let first = rig.file("first.wav", &constant(0.5, 14_400))?;
    let second = rig.file("second.wav", &constant(-0.25, 14_400))?;
    rig.control(MusicCommand::SetRepeat(RepeatMode::Off))
        .await?;
    rig.control(MusicCommand::SetCrossfade(CrossfadeDuration::try_new(
        Duration::ZERO,
    )?))
    .await?;

    rig.play(vec![entry(first, "First"), entry(second, "Second")])
        .await?;
    rig.wait_for(|state| playing_title(state) == Some("Second"))
        .await?;
    rig.wait_for(|state| state.music.is_none()).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let captures = rig.sound_card.captures();
    assert_eq!(captures.len(), 1);
    // The local output resamples the 48 kHz files to 44.1 kHz: 0.3 s is 13 230 frames each.
    let played = from_first_sound(&captures[0]);
    assert!(within(&played[100..13_100], 0.5, 0.01));
    assert!(within(&played[13_400..26_300], -0.25, 0.01));
    assert!(within(&played[26_600..], 0.0, 0.01));
    assert_eq!(rig.player.diagnostics().music_underrun_frames, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queued_song_plays_between_two_tracks_without_a_gap() -> anyhow::Result<()> {
    let mut rig = Rig::start()?;
    let first = rig.file("first.wav", &constant(0.5, 14_400))?;
    let second = rig.file("second.wav", &constant(-0.25, 14_400))?;
    let boss = queued(rig.file("boss.wav", &constant(0.75, 14_400))?, "Boss");
    rig.control(MusicCommand::SetRepeat(RepeatMode::Off))
        .await?;
    rig.control(MusicCommand::SetCrossfade(CrossfadeDuration::try_new(
        Duration::ZERO,
    )?))
    .await?;

    rig.play(vec![entry(first, "First"), entry(second, "Second")])
        .await?;
    rig.player.add_to_queue(boss.clone()).await?;
    let queued_ids = rig.state().queued_entry_ids;
    let during_boss = rig
        .wait_for(|state| playing_title(state) == Some("Boss"))
        .await?;
    rig.wait_for(|state| playing_title(state) == Some("Second"))
        .await?;
    rig.wait_for(|state| state.music.is_none()).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(queued_ids, [boss.entry.entry_id]);
    assert_eq!(
        during_boss.queued_entry_ids,
        Vec::<CollectionEntryId>::new()
    );
    assert_eq!(
        during_boss.music.map(|music| music.collection_id),
        Some(boss.collection_id)
    );
    let captures = rig.sound_card.captures();
    let played = from_first_sound(&captures[0]);
    assert!(within(&played[100..13_100], 0.5, 0.01));
    assert!(within(&played[13_400..26_300], 0.75, 0.01));
    assert!(within(&played[26_600..39_500], -0.25, 0.01));
    assert_eq!(rig.player.diagnostics().music_underrun_frames, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn queued_songs_and_repeating_one_track_exclude_each_other() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let main = rig.file("main.wav", &constant(0.1, THREE_SECONDS))?;
    let boss = queued(rig.file("boss.wav", &constant(0.5, THREE_SECONDS))?, "Boss");

    let before_music = rig.player.add_to_queue(boss.clone()).await;
    rig.play(vec![entry(main, "Main")]).await?;
    rig.player.add_to_queue(boss.clone()).await?;
    let repeating_track = rig
        .player
        .control_music(MusicCommand::SetRepeat(RepeatMode::Track))
        .await;
    let repeat_while_queued = rig.state().modes.repeat;
    rig.player.remove_from_queue(boss.entry.entry_id).await;
    rig.control(MusicCommand::SetRepeat(RepeatMode::Track))
        .await?;
    let while_repeating_track = rig.player.add_to_queue(boss).await;

    assert_eq!(before_music, Err(QueueSongError::NothingPlaying));
    assert!(matches!(
        repeating_track,
        Err(ControlMusicError::SongsQueued)
    ));
    assert_eq!(repeat_while_queued, RepeatMode::Playlist);
    assert_eq!(while_repeating_track, Err(QueueSongError::RepeatingTrack));
    assert_eq!(
        rig.state().queued_entry_ids,
        Vec::<CollectionEntryId>::new()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn queuing_with_nothing_playing_starts_the_song_in_its_playlist() -> anyhow::Result<()> {
    let mut rig = Rig::start()?;
    let folder = rig.directory.path().join("combat");
    std::fs::create_dir_all(&folder)?;
    write_wav(
        &folder.join("ambush.wav"),
        SAMPLE_RATE,
        &constant(0.2, THREE_SECONDS),
    )?;
    write_wav(
        &folder.join("boss.wav"),
        SAMPLE_RATE,
        &constant(0.3, THREE_SECONDS),
    )?;
    let database = open_database(&rig.directory.path().join("tufo.sqlite3")).await?;
    let library = Library::open(database, DiskLibraryFiles).await?;
    import_paths::execute(
        ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![LocalPath::try_new(folder)?],
        },
        &library,
    )
    .await?;
    let playlist = library.watch().borrow().collections[0].clone();
    let boss = playlist
        .entries
        .iter()
        .find(|entry| entry.title.as_str() == "boss")
        .context("the playlist lists the boss track")?
        .id;

    add_to_queue::execute(boss, &library, rig.player.as_ref()).await?;

    let state = rig.wait_for(|state| state.music.is_some()).await?;
    let music = state.music.context("the song plays")?;
    assert_eq!(music.entry.entry_id, boss);
    assert_eq!(music.collection_id, playlist.id);
    assert_eq!(state.queued_entry_ids, Vec::<CollectionEntryId>::new());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn next_skips_to_the_following_track() -> anyhow::Result<()> {
    let mut rig = Rig::start()?;
    let first = rig.file("first.wav", &constant(0.5, THREE_SECONDS))?;
    let second = rig.file("second.wav", &constant(-0.25, THREE_SECONDS))?;
    rig.play(vec![entry(first, "First"), entry(second, "Second")])
        .await?;
    rig.wait_for(|state| playing_title(state) == Some("First"))
        .await?;

    rig.control(MusicCommand::Next).await?;

    let state = rig
        .wait_for(|state| playing_title(state) == Some("Second"))
        .await?;
    assert!(position_seconds(&state)? < 0.5);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_outputs_carries_the_music_over_at_its_position() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let track = rig.file("ramp.wav", &ramp(THREE_SECONDS))?;
    rig.play_once(entry(track, "Ramp")).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    rig.player.select_output(voice_channel()).await?;
    let state = rig.state();
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(state.output, voice_channel());
    assert_eq!(state.voice, VoiceConnectionState::Connected);
    let switched_at = position_seconds(&state)?;
    assert!(
        (0.5..1.6).contains(&switched_at),
        "switched at {switched_at} s"
    );
    let captures = rig.sound_card.captures();
    assert_eq!(captures.len(), 2);
    let resumed_at = resumed_ramp_position(&captures[1])?;
    assert!(
        (resumed_at - switched_at).abs() < 0.1,
        "switched at {switched_at} s, resumed at {resumed_at} s"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_voice_connection_rejoins_and_resumes_the_music() -> anyhow::Result<()> {
    let mut rig = Rig::start()?;
    rig.player.select_output(voice_channel()).await?;
    let track = rig.file("ramp.wav", &ramp(THREE_SECONDS))?;
    rig.play_once(entry(track, "Ramp")).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    rig.controls.drop_voice_connection()?;
    rig.wait_for(|state| matches!(state.voice, VoiceConnectionState::Reconnecting { .. }))
        .await?;
    let state = rig
        .wait_for(|state| state.voice == VoiceConnectionState::Connected)
        .await?;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let rejoined_at = position_seconds(&state)?;
    assert!(rejoined_at >= 0.3, "rejoined at {rejoined_at} s");
    let captures = rig.sound_card.captures();
    assert_eq!(captures.len(), 2);
    let resumed_at = resumed_ramp_position(&captures[1])?;
    assert!(
        (resumed_at - rejoined_at).abs() < 0.1,
        "rejoined at {rejoined_at} s, resumed at {resumed_at} s"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn rejoining_gives_up_after_the_policy_attempts() -> anyhow::Result<()> {
    let mut rig = Rig::with_timings(EngineTimings {
        reconnect_policy: ReconnectPolicy {
            attempts_max: 2,
            delay_min: Duration::from_millis(10),
            delay_max: Duration::from_millis(10),
        },
        ..EngineTimings::default()
    })?;
    rig.player.select_output(voice_channel()).await?;
    rig.controls.fail_voice_opens.store(true, Ordering::Relaxed);

    rig.controls.drop_voice_connection()?;
    let state = rig
        .wait_for(|state| {
            matches!(
                state.voice,
                VoiceConnectionState::Disconnected { reason: Some(_) }
            )
        })
        .await?;

    assert_eq!(state.output, voice_channel());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn disabling_discord_leaves_the_channel_and_blocks_new_ones() -> anyhow::Result<()> {
    let mut rig = Rig::start()?;
    rig.player.select_output(voice_channel()).await?;
    rig.wait_for(|state| state.voice == VoiceConnectionState::Connected)
        .await?;

    rig.controls.voice_disabled.store(true, Ordering::Relaxed);
    rig.player.select_output(OutputTarget::Local).await?;
    rig.player.refresh_voice_availability().await;
    let state = rig
        .wait_for(|state| state.voice == VoiceConnectionState::Disabled)
        .await?;

    assert_eq!(state.output, OutputTarget::Local);
    let joined_while_disabled = rig.player.select_output(voice_channel()).await;
    assert!(matches!(
        joined_while_disabled,
        Err(SelectOutputError::DiscordDisabled)
    ));

    rig.controls.voice_disabled.store(false, Ordering::Relaxed);
    rig.player.refresh_voice_availability().await;
    rig.wait_for(|state| matches!(state.voice, VoiceConnectionState::Disconnected { .. }))
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn one_shot_sounds_end_while_looping_sounds_follow_the_output() -> anyhow::Result<()> {
    let mut rig = Rig::start()?;
    let door = SoundCue {
        entry_id: CollectionEntryId::from_uuid(Uuid::now_v7()),
        path: rig.file("door.wav", &constant(0.3, 9_600))?,
        settings: SoundSettings::default(),
    };
    let rain = SoundCue {
        entry_id: CollectionEntryId::from_uuid(Uuid::now_v7()),
        path: rig.file("rain.wav", &constant(0.2, 9_600))?,
        settings: SoundSettings {
            looping: true,
            ..SoundSettings::default()
        },
    };
    let rain_id = rain.entry_id;

    rig.player.play_sound(door).await?;
    rig.player.play_sound(rain).await?;
    let both = rig.state().sounds.len();
    rig.wait_for(|state| state.sounds.len() == 1).await?;
    rig.player.select_output(voice_channel()).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after_switch = rig.state().sounds;
    rig.player.stop_sound(rain_id).await;

    assert_eq!(both, 2);
    assert_eq!(after_switch.len(), 1);
    assert_eq!(after_switch[0].entry_id, rain_id);
    let captures = rig.sound_card.captures();
    let looped = from_first_sound(&captures[1]);
    assert!(within(
        &looped[AFTER_FADE_IN_FRAMES..AFTER_FADE_IN_FRAMES + 4_800],
        0.2,
        0.05
    ));
    assert_eq!(rig.state().sounds, Vec::<ActiveSound>::new());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn firing_a_playing_sound_again_restarts_it() -> anyhow::Result<()> {
    let mut rig = Rig::start()?;
    let horn = SoundCue {
        entry_id: CollectionEntryId::from_uuid(Uuid::now_v7()),
        path: rig.file("horn.wav", &constant(0.3, SAMPLE_RATE as usize))?,
        settings: SoundSettings::default(),
    };

    rig.player.play_sound(horn.clone()).await?;
    let first_start = rig.state().sounds[0].started_at;
    tokio::time::sleep(Duration::from_millis(100)).await;
    rig.player.play_sound(horn).await?;
    let state = rig.wait_for(|state| state.sounds.len() == 1).await?;

    assert!(state.sounds[0].started_at > first_start);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn seeking_and_pausing_move_and_hold_the_position() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let track = rig.file("ramp.wav", &ramp(THREE_SECONDS))?;
    rig.play_once(entry(track, "Ramp")).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;

    rig.control(MusicCommand::Seek(Duration::from_secs(2)))
        .await?;
    let sought = rig.state();
    rig.control(MusicCommand::Pause).await?;
    let paused = rig.state();
    tokio::time::sleep(Duration::from_millis(300)).await;
    rig.control(MusicCommand::Resume).await?;
    let resumed = rig.state();

    assert!((position_seconds(&sought)? - 2.0).abs() < 0.1);
    assert!(paused.music.as_ref().is_some_and(|music| music.paused));
    // The pause fades out over 100 ms, and the track keeps playing while it fades.
    assert!((position_seconds(&resumed)? - position_seconds(&paused)?).abs() < 0.15);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn playing_while_paused_starts_the_new_track_without_crossfading() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let paused_track = rig.file("paused.wav", &constant(0.5, THREE_SECONDS))?;
    let next_track = rig.file("next.wav", &constant(-0.25, THREE_SECONDS))?;
    rig.control(MusicCommand::SetCrossfade(CrossfadeDuration::try_new(
        Duration::from_secs(1),
    )?))
    .await?;
    rig.play_once(entry(paused_track, "Paused")).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    rig.control(MusicCommand::Pause).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let paused_frames = rig.sound_card.captures()[0].len();

    rig.play_once(entry(next_track, "Next")).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let captures = rig.sound_card.captures();
    let after_pause = &captures[0][paused_frames..];
    // Any trace of the paused track lifts the recording above zero.
    assert!(after_pause.iter().all(|sample| *sample < 0.01));
    // The resume fade lasts 20 ms, 882 frames; a crossfade would still be fading in.
    let played = from_first_sound(after_pause)
        .get(2_000..4_000)
        .context("the output played too little")?;
    assert!(within(played, -0.25, 0.01));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn standing_by_fades_everything_out_and_leaves_the_voice_channel() -> anyhow::Result<()> {
    // The renderer's frame clock can lag wall time; capture 300 ms around the 250 ms fade.
    let capture_frames = SAMPLE_RATE as usize * 3 / 10;
    let mut rig = Rig::start()?;
    rig.player.select_output(voice_channel()).await?;
    let track = rig.file("music.wav", &constant(0.5, THREE_SECONDS))?;
    let rain = SoundCue {
        entry_id: CollectionEntryId::from_uuid(Uuid::now_v7()),
        path: rig.file("rain.wav", &constant(0.2, 9_600))?,
        settings: SoundSettings {
            looping: true,
            ..SoundSettings::default()
        },
    };
    rig.play_once(entry(track, "Music")).await?;
    rig.player.play_sound(rain).await?;
    rig.wait_for_recorded_frames(0, rig.recorded_frames(0) + capture_frames)
        .await?;
    let before = rig.sound_card.captures();
    let stood_by_at = before[0].len();
    assert!(within(&before[0][stood_by_at - 480..], 0.7, 0.01));

    rig.player.stand_by().await;
    let faded_by = rig.recorded_frames(0) + capture_frames;
    let state = rig
        .wait_for(|state| state.music.is_none() && state.sounds.is_empty())
        .await?;
    rig.wait_for_closes(1).await?;
    let released = rig
        .wait_for(|state| state.output == OutputTarget::Local)
        .await?;
    rig.wait_for_recorded_frames(0, faded_by).await?;

    assert_eq!(state.output, voice_channel());
    assert_eq!(
        released.voice,
        VoiceConnectionState::Disconnected { reason: None }
    );
    let after = &rig.sound_card.captures()[0][stood_by_at..];
    // The 250 ms fade passes between full level, 0.7, and silence instead of cutting.
    assert!(after.iter().any(|sample| (0.1..0.6).contains(sample)));
    assert!(within(&after[after.len() - 480..], 0.0, 0.01));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_silent_local_device_closes_after_the_idle_delay_and_reopens_to_play()
-> anyhow::Result<()> {
    let mut rig = Rig::with_timings(EngineTimings {
        idle_release_delay: Duration::from_millis(200),
        ..EngineTimings::default()
    })?;
    let blip = rig.file("blip.wav", &constant(0.5, 4_800))?;
    rig.play_once(entry(blip.clone(), "Blip")).await?;
    rig.wait_for(|state| state.music.is_none()).await?;

    rig.wait_for_closes(1).await?;
    rig.play_once(entry(blip, "Blip")).await?;
    rig.wait_for(|state| playing_title(state) == Some("Blip"))
        .await?;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(rig.state().output, OutputTarget::Local);
    assert_eq!(rig.sound_card.captures().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn selecting_a_voice_channel_while_idle_keeps_it_past_the_idle_delay() -> anyhow::Result<()> {
    let mut rig = Rig::with_timings(EngineTimings {
        idle_release_delay: Duration::from_millis(300),
        ..EngineTimings::default()
    })?;
    let blip = rig.file("blip.wav", &constant(0.5, 4_800))?;
    rig.play_once(entry(blip, "Blip")).await?;
    rig.wait_for(|state| state.music.is_none()).await?;
    // A tick, every 50 ms, plans the idle release meanwhile.
    tokio::time::sleep(Duration::from_millis(100)).await;

    rig.player.select_output(voice_channel()).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(rig.state().output, voice_channel());
    assert_eq!(rig.state().voice, VoiceConnectionState::Connected);
    assert_eq!(rig.closes(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn paused_sounds_fall_silent_until_they_resume() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let rain = sound(rig.file("rain.wav", &constant(0.2, 9_600))?, true);
    rig.player.play_sound(rain).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;

    rig.player.control_sounds(SoundsCommand::Pause).await;
    let paused = rig.state();
    let paused_at = rig.recorded_frames(0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    rig.player.control_sounds(SoundsCommand::Resume).await;
    let resumed_at = rig.recorded_frames(0);
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(paused.sounds_paused);
    assert!(!rig.state().sounds_paused);
    let capture = &rig.sound_card.captures()[0];
    assert!(within(
        &capture[paused_at + SOUNDS_PAUSE_FADE_FRAMES..resumed_at],
        0.0,
        1e-4
    ));
    let after_resume = resumed_at + SOUNDS_PAUSE_FADE_FRAMES;
    assert!(within(
        &capture[after_resume..after_resume + 4_410],
        0.2,
        0.05
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn paused_sounds_stay_paused_on_a_new_output_until_another_fires() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let rain = sound(rig.file("rain.wav", &constant(0.2, 9_600))?, true);
    let door = sound(rig.file("door.wav", &constant(0.3, 48_000))?, false);

    rig.player.control_sounds(SoundsCommand::Pause).await;
    let nothing_to_pause = rig.state();
    rig.player.play_sound(rain).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    rig.player.control_sounds(SoundsCommand::Pause).await;
    rig.player.select_output(voice_channel()).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let on_voice = rig.sound_card.captures()[1].clone();
    rig.player.play_sound(door).await?;
    let fired = rig.state();

    assert!(!nothing_to_pause.sounds_paused);
    assert_ne!(on_voice, Vec::<f32>::new());
    assert!(within(&on_voice, 0.0, 1e-4));
    assert!(!fired.sounds_paused);
    assert_eq!(fired.sounds.len(), 2);
    Ok(())
}

/// A library holding one soundboard of a looping rain and a one-shot door, and their entries.
async fn rain_and_door_library(
    rig: &Rig,
) -> anyhow::Result<(
    Library<DiskLibraryFiles>,
    CollectionEntryId,
    CollectionEntryId,
)> {
    let folder = rig.directory.path().join("sfx");
    std::fs::create_dir_all(&folder)?;
    write_wav(&folder.join("rain.wav"), SAMPLE_RATE, &constant(0.2, 9_600))?;
    write_wav(&folder.join("door.wav"), SAMPLE_RATE, &constant(0.3, 9_600))?;
    let database = open_database(&rig.directory.path().join("tufo.sqlite3")).await?;
    let library = Library::open(database, DiskLibraryFiles).await?;
    import_paths::execute(
        ImportPaths {
            kind: CollectionKind::Soundboard,
            paths: vec![LocalPath::try_new(folder)?],
        },
        &library,
    )
    .await?;
    let snapshot = library.watch().borrow().clone();
    let entry_titled = |title: &str| {
        snapshot.collections[0]
            .entries
            .iter()
            .find(|entry| entry.title.as_str() == title)
            .map(|entry| (entry.id, entry.sound))
            .context("the soundboard lists the sound")
    };
    let (rain, rain_sound) = entry_titled("rain")?;
    let (door, _) = entry_titled("door")?;
    let looping = UpdateCollectionEntry {
        sound: Some(SoundSettings {
            looping: true,
            ..rain_sound
        }),
        ..UpdateCollectionEntry::default()
    };
    update_collection_entry::execute(rain, looping, &library, rig.player.as_ref()).await?;
    Ok((library, rain, door))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_login_resumes_the_loops_left_playing_unless_they_were_paused() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let (library, rain, door) = rain_and_door_library(&rig).await?;
    let player = rig.player.as_ref();

    play_entry_sound::execute(rain, &library, player).await?;
    play_entry_sound::execute(door, &library, player).await?;
    player.stand_by().await;
    let resumed = resume_saved_loops::execute(&library, player).await?;
    let playing_after_resume = rig.state().sounds;
    control_sounds::execute(SoundsCommand::Pause, player, library.database()).await;
    player.stand_by().await;
    let left_paused = resume_saved_loops::execute(&library, player).await?;
    let playing_after_paused = rig.state().sounds;
    play_entry_sound::execute(rain, &library, player).await?;
    stop_sound::execute(rain, player, library.database()).await;
    let stopped = resume_saved_loops::execute(&library, player).await?;

    assert_eq!(resumed, ResumeSavedLoopsOutcome::Resumed { loops: 1 });
    assert_eq!(playing_after_resume.len(), 1);
    assert_eq!(playing_after_resume[0].entry_id, rain);
    assert_eq!(left_paused, ResumeSavedLoopsOutcome::LeftPaused);
    assert_eq!(playing_after_paused, Vec::<ActiveSound>::new());
    assert_eq!(stopped, ResumeSavedLoopsOutcome::NothingSaved);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_an_entry_updates_the_library_and_the_active_sound() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let (library, rain, _) = rain_and_door_library(&rig).await?;
    let player = rig.player.as_ref();
    play_entry_sound::execute(rain, &library, player).await?;
    control_sounds::execute(SoundsCommand::Pause, player, library.database()).await;

    update_collection_entry::execute(
        rain,
        UpdateCollectionEntry {
            sound: Some(SoundSettings {
                looping: false,
                ..SoundSettings::default()
            }),
            ..UpdateCollectionEntry::default()
        },
        &library,
        player,
    )
    .await?;

    let state = rig.state();
    let active = state
        .sounds
        .iter()
        .find(|sound| sound.entry_id == rain)
        .context("the paused sound remains active")?;
    assert!(!active.looping);
    let snapshot = library.watch().borrow().clone();
    let saved = snapshot
        .collections
        .iter()
        .flat_map(|collection| &collection.entries)
        .find(|entry| entry.id == rain)
        .context("the entry remains in the library")?;
    assert!(!saved.sound.looping);
    player.shut_down().await;
    Ok(())
}

/// A three-second ramp's region: the intro from 0.2 s to 1 s plays once, then 1 s to 2 s repeats.
fn ramp_region() -> anyhow::Result<LoopRegion> {
    Ok(LoopRegion::try_new(
        SourceTiming {
            sample_rate: NonZeroU32::new(SAMPLE_RATE).context("the sample rate is zero")?,
            frame_count: SourceFrames(u64::try_from(THREE_SECONDS)?),
        },
        LoopMarkers {
            playback_start: SourceFrames(9_600),
            loop_start: SourceFrames(48_000),
            loop_end: SourceFrames(96_000),
            crossfade: SourceFrames::ZERO,
        },
    )?)
}

/// [`ramp_region`]'s first wrap and pass length at the local output's 44.1 kHz.
const RAMP_REGION_FIRST_WRAP_FRAME: usize = 79_380;
const RAMP_REGION_PASS_FRAMES: usize = 44_100;
/// The resampler smooths a jump over a few dozen frames.
const RESAMPLED_JUMP_FRAMES: usize = 64;

fn ramp_wraps(played: &[f32]) -> Vec<usize> {
    played
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[1] < pair[0] - 0.05)
        .map(|(index, _)| index + 1)
        .collect()
}

fn ramp_seconds_at(played: &[f32], frame: usize) -> f32 {
    ramp_position_seconds(played[frame])
}

impl Rig {
    /// Waits for `frames` frames after the output's first sound.
    async fn wait_for_played_frames(&self, output: usize, frames: usize) -> anyhow::Result<()> {
        tokio::time::timeout(WAIT_TIMEOUT, async {
            while self
                .sound_card
                .captures()
                .get(output)
                .is_none_or(|capture| from_first_sound(capture).len() < frames)
            {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .with_context(|| format!("output {output} never played {frames} frames"))
    }

    /// Pausing publishes a fresh position sample.
    async fn paused_state(&self) -> anyhow::Result<PlayerState> {
        self.control(MusicCommand::Pause).await?;
        Ok(self.state())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn repeating_a_track_plays_its_intro_once_then_repeats_its_loop() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let looped = entry(rig.file("boss.wav", &ramp(THREE_SECONDS))?, "Boss");
    rig.save_loop(&looped, ramp_region()?);
    rig.control(MusicCommand::SetRepeat(RepeatMode::Track))
        .await?;

    rig.play(vec![looped]).await?;
    rig.wait_for_played_frames(
        0,
        RAMP_REGION_FIRST_WRAP_FRAME + 3 * RAMP_REGION_PASS_FRAMES,
    )
    .await?;
    let paused = rig.paused_state().await?;

    let captures = rig.sound_card.captures();
    let played = from_first_sound(&captures[0]);
    assert!((ramp_seconds_at(played, 100) - 0.2).abs() < 0.01);
    let wraps = ramp_wraps(played);
    let expected_wraps =
        (0..3).map(|pass| RAMP_REGION_FIRST_WRAP_FRAME + pass * RAMP_REGION_PASS_FRAMES);
    assert!(
        wraps
            .iter()
            .zip(expected_wraps)
            .all(|(wrap, expected)| *wrap == expected),
        "wrapped at {wraps:?}"
    );
    for wrap in wraps.iter().take(3) {
        assert!((ramp_seconds_at(played, wrap - RESAMPLED_JUMP_FRAMES) - 2.0).abs() < 0.01);
        assert!((ramp_seconds_at(played, wrap + RESAMPLED_JUMP_FRAMES) - 1.0).abs() < 0.01);
    }
    let position = position_seconds(&paused)?;
    assert!((1.0..2.0).contains(&position), "at {position} s");
    assert_eq!(rig.player.diagnostics().music_underrun_frames, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn turning_repeat_off_finishes_the_loop_pass_then_moves_on() -> anyhow::Result<()> {
    let mut rig = Rig::start()?;
    let looped = entry(rig.file("boss.wav", &ramp(THREE_SECONDS))?, "Boss");
    let next = entry(
        rig.file("next.wav", &constant(-0.25, THREE_SECONDS))?,
        "Next",
    );
    rig.save_loop(&looped, ramp_region()?);
    rig.control(MusicCommand::SetCrossfade(CrossfadeDuration::try_new(
        Duration::ZERO,
    )?))
    .await?;
    rig.control(MusicCommand::SetRepeat(RepeatMode::Track))
        .await?;
    rig.play(vec![looped, next]).await?;
    rig.wait_for_played_frames(
        0,
        RAMP_REGION_FIRST_WRAP_FRAME + RAMP_REGION_PASS_FRAMES / 2,
    )
    .await?;

    rig.control(MusicCommand::SetRepeat(RepeatMode::Off))
        .await?;
    rig.wait_for(|state| playing_title(state) == Some("Next"))
        .await?;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let captures = rig.sound_card.captures();
    let played = from_first_sound(&captures[0]);
    let next_starts = played
        .iter()
        .position(|sample| (sample + 0.25).abs() < 0.01)
        .context("the next track never played")?;
    let pass_end = RAMP_REGION_FIRST_WRAP_FRAME + RAMP_REGION_PASS_FRAMES;
    assert!(next_starts.abs_diff(pass_end) < RESAMPLED_JUMP_FRAMES);
    assert_eq!(
        ramp_wraps(&played[..pass_end - RESAMPLED_JUMP_FRAMES]),
        [RAMP_REGION_FIRST_WRAP_FRAME]
    );
    assert!((ramp_seconds_at(played, pass_end - RESAMPLED_JUMP_FRAMES) - 2.0).abs() < 0.01);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_song_that_does_not_repeat_plays_from_its_playback_start_to_its_loop_end()
-> anyhow::Result<()> {
    let mut rig = Rig::start()?;
    let looped = entry(rig.file("boss.wav", &ramp(THREE_SECONDS))?, "Boss");
    let next = entry(rig.file("next.wav", &constant(-0.25, 14_400))?, "Next");
    rig.save_loop(&looped, ramp_region()?);
    rig.control(MusicCommand::SetCrossfade(CrossfadeDuration::try_new(
        Duration::ZERO,
    )?))
    .await?;
    rig.control(MusicCommand::SetRepeat(RepeatMode::Off))
        .await?;

    rig.play(vec![looped, next]).await?;
    rig.wait_for(|state| playing_title(state) == Some("Next"))
        .await?;
    rig.wait_for(|state| state.music.is_none()).await?;

    let captures = rig.sound_card.captures();
    let played = from_first_sound(&captures[0]);
    let next_starts = played
        .iter()
        .position(|sample| (sample + 0.25).abs() < 0.01)
        .context("the next track never played")?;
    assert!(next_starts.abs_diff(RAMP_REGION_FIRST_WRAP_FRAME) < RESAMPLED_JUMP_FRAMES);
    assert_eq!(
        ramp_wraps(&played[..RAMP_REGION_FIRST_WRAP_FRAME - RESAMPLED_JUMP_FRAMES]),
        Vec::<usize>::new()
    );
    assert!((ramp_seconds_at(played, 100) - 0.2).abs() < 0.01);
    assert!(
        (ramp_seconds_at(played, RAMP_REGION_FIRST_WRAP_FRAME - RESAMPLED_JUMP_FRAMES) - 2.0).abs()
            < 0.01
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn seeks_stay_between_the_playback_start_and_the_loop_end() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let looped = entry(rig.file("boss.wav", &ramp(THREE_SECONDS))?, "Boss");
    rig.save_loop(&looped, ramp_region()?);
    rig.control(MusicCommand::SetRepeat(RepeatMode::Track))
        .await?;
    rig.play(vec![looped]).await?;

    rig.control(MusicCommand::Seek(Duration::from_millis(50)))
        .await?;
    let before_playback_start = position_seconds(&rig.state())?;
    let past_loop_end = rig
        .player
        .control_music(MusicCommand::Seek(Duration::from_millis(2_500)))
        .await;

    assert!((before_playback_start - 0.2).abs() < 0.05);
    assert!(matches!(past_loop_end, Err(ControlMusicError::SeekPastEnd)));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn restarting_a_song_replays_its_intro_with_the_loop_saved_since() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let song = entry(rig.file("boss.wav", &ramp(THREE_SECONDS))?, "Boss");
    rig.control(MusicCommand::SetRepeat(RepeatMode::Track))
        .await?;
    rig.play(vec![song.clone()]).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    rig.save_loop(&song, ramp_region()?);
    let before_restart = rig.state();
    rig.control(MusicCommand::Next).await?;
    let restarted = rig.state();

    assert_eq!(
        before_restart.music.and_then(|music| music.loop_region),
        None
    );
    let music = restarted.music.context("the song stopped")?;
    assert_eq!(music.loop_region, Some(ramp_region()?));
    assert!((music.position.position.as_secs_f32() - 0.2).abs() < 0.05);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_outputs_inside_a_loop_resumes_at_its_position_and_keeps_looping()
-> anyhow::Result<()> {
    let rig = Rig::start()?;
    let looped = entry(rig.file("boss.wav", &ramp(THREE_SECONDS))?, "Boss");
    rig.save_loop(&looped, ramp_region()?);
    rig.control(MusicCommand::SetRepeat(RepeatMode::Track))
        .await?;
    rig.play(vec![looped]).await?;
    rig.wait_for_played_frames(
        0,
        RAMP_REGION_FIRST_WRAP_FRAME + RAMP_REGION_PASS_FRAMES / 4,
    )
    .await?;

    rig.player.select_output(voice_channel()).await?;
    let switched_at = position_seconds(&rig.state())?;
    rig.wait_for_played_frames(1, SAMPLE_RATE as usize * 2)
        .await?;

    assert!((1.0..1.6).contains(&switched_at), "at {switched_at} s");
    let captures = rig.sound_card.captures();
    let resumed_at = resumed_ramp_position(&captures[1])?;
    assert!(
        (resumed_at - switched_at).abs() < 0.1,
        "switched at {switched_at} s, resumed at {resumed_at} s"
    );
    let resumed = from_first_sound(&captures[1]);
    let wraps = ramp_wraps(resumed);
    assert_ne!(wraps, Vec::<usize>::new());
    for wrap in wraps {
        assert!((ramp_seconds_at(resumed, wrap - 1) - 2.0).abs() < 0.01);
        assert!((ramp_seconds_at(resumed, wrap) - 1.0).abs() < 0.01);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_extra_song_repeats_its_loop_while_it_loops() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let main = rig.file("main.wav", &constant(0.1, THREE_SECONDS * 3))?;
    let extra = layer(rig.file("boss.wav", &ramp(THREE_SECONDS))?, "Boss");
    rig.save_loop(&extra.entry, ramp_region()?);
    rig.play_once(entry(main, "Main")).await?;
    rig.player.add_music_layer(extra.clone()).await?;
    rig.player
        .update_music_layer(
            extra.entry.entry_id,
            tufo_wire::player::UpdateMusicLayer {
                looping: Some(true),
                ..Default::default()
            },
        )
        .await;

    tokio::time::sleep(Duration::from_millis(3_200)).await;
    let paused = rig.paused_state().await?;

    let (_, layer_state) = paused
        .music_layers
        .iter()
        .next()
        .context("the extra song stopped")?;
    let position = layer_state.music.position.position.as_secs_f32();
    assert!((1.0..2.0).contains(&position), "at {position} s");
    assert_eq!(layer_state.music.loop_region, Some(ramp_region()?));
    Ok(())
}

fn seam_preview(path: AudioFilePath) -> anyhow::Result<SongPreview> {
    Ok(SongPreview {
        path,
        draft: ramp_region()?,
        mode: PreviewMode::Seam(SeamPreviewSide::try_new(Duration::from_millis(300))?),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_seam_preview_pauses_local_music_and_resumes_it_when_it_stops() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let song = rig.file("boss.wav", &ramp(THREE_SECONDS))?;
    rig.play_once(entry(song.clone(), "Boss")).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let session = rig.player.start_preview(seam_preview(song)?).await?;
    let during = rig.state();
    rig.wait_for_played_frames(1, 44_100).await?;
    rig.player.stop_preview(session.id).await;
    let after = rig.state();

    assert_eq!(
        session.started.seam_cycle,
        Some(Duration::from_millis(300 + 300 + 750))
    );
    assert!(during.music.as_ref().is_some_and(|music| music.paused));
    assert!(after.music.as_ref().is_some_and(|music| !music.paused));
    let captures = rig.sound_card.captures();
    let preview = from_first_sound(&captures[1]);
    // The clip plays 1.7 s to 2 s of the ramp, then 1 s to 1.3 s, at 44.1 kHz.
    assert!((ramp_seconds_at(preview, 4_410) - 1.8).abs() < 0.01);
    assert!((ramp_seconds_at(preview, 13_230 + 4_410) - 1.1).abs() < 0.01);
    assert!(session.ended.await.is_err());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_preview_ends_the_previous_one_and_keeps_the_music_paused() -> anyhow::Result<()> {
    let rig = Rig::start()?;
    let song = rig.file("boss.wav", &ramp(THREE_SECONDS))?;
    rig.play_once(entry(song.clone(), "Boss")).await?;

    let first = rig
        .player
        .start_preview(seam_preview(song.clone())?)
        .await?;
    let second = rig.player.start_preview(seam_preview(song)?).await?;
    let first_ended = tokio::time::timeout(WAIT_TIMEOUT, first.ended).await;
    rig.player.stop_preview(first.id).await;
    let after_stale_stop = rig.state();
    rig.player.stop_preview(second.id).await;

    assert!(first_ended.is_ok());
    assert!(
        after_stale_stop
            .music
            .as_ref()
            .is_some_and(|music| music.paused)
    );
    assert!(
        rig.state()
            .music
            .as_ref()
            .is_some_and(|music| !music.paused)
    );
    Ok(())
}
