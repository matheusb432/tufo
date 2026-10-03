//! The audio engine capability the playback interactors drive.

use std::future::Future;

use tokio::sync::{oneshot, watch};
use tufo_models::{
    library::{AudioFileId, CollectionEntryId, SoundSettings, song_loop::LoopRegion},
    playback::{
        AudioFilePath, OutputTarget,
        diagnostics::PlayerDiagnostics,
        player_state::{PlayerState, SoundCue},
        queue::{QueueStart, QueuedEntry},
    },
};
use tufo_wire::song_loops::{PreviewMode, PreviewStarted};

use super::{
    add_music_layer::{AddMusicLayerError, MusicLayerCue},
    add_to_queue::QueueSongError,
    control_music::{ControlMusicError, MusicCommand},
    control_sounds::SoundsCommand,
    play_queue::PlayQueueError,
    select_output::SelectOutputError,
};
use crate::error::ErrorMeta;

/// Plays a music queue with up to two extra songs and any number of sound effects on one output,
/// and publishes what it plays.
pub trait Player: Send + Sync {
    /// Adds one song, using the main track while no queue exists. Active songs are left playing;
    /// a fourth song fails.
    fn add_music_layer(
        &self,
        cue: MusicLayerCue,
    ) -> impl Future<Output = Result<(), AddMusicLayerError>> + Send;

    /// Stops one song, dropping the queue if it is the main track. Absent songs are ignored.
    fn stop_music_layer(&self, entry_id: CollectionEntryId) -> impl Future<Output = ()> + Send;

    /// Updates an extra song's settings; absent songs and the main track are ignored.
    fn update_music_layer(
        &self,
        entry_id: CollectionEntryId,
        update: tufo_wire::player::UpdateMusicLayer,
    ) -> impl Future<Output = ()> + Send;

    /// Replaces the music with a queue of `start`'s tracks, applying the player's shuffle and
    /// repeat modes, and plays its start track.
    fn play_queue(
        &self,
        start: QueueStart,
    ) -> impl Future<Output = Result<(), PlayQueueError>> + Send;

    /// Queues `song` after the songs already queued; a queued song stays in place.
    fn add_to_queue(
        &self,
        song: QueuedEntry,
    ) -> impl Future<Output = Result<(), QueueSongError>> + Send;

    /// Takes a song out of the queue. Songs that aren't queued are ignored.
    fn remove_from_queue(&self, entry_id: CollectionEntryId) -> impl Future<Output = ()> + Send;

    /// Pauses, resumes, stops, skips, seeks, or changes a music setting.
    fn control_music(
        &self,
        command: MusicCommand,
    ) -> impl Future<Output = Result<(), ControlMusicError>> + Send;

    /// Fires `cue` over the music, restarting it when that entry is already playing.
    fn play_sound(&self, cue: SoundCue) -> impl Future<Output = Result<(), PlayAudioError>> + Send;

    /// Firing a sound while the sounds are paused resumes them.
    fn control_sounds(&self, command: SoundsCommand) -> impl Future<Output = ()> + Send;

    /// Fades a playing sound out over its fade-out. Stopping a silent entry does nothing.
    fn stop_sound(&self, entry_id: CollectionEntryId) -> impl Future<Output = ()> + Send;

    /// Applies new volume and looping settings to a playing sound. A silent entry is ignored.
    fn update_sound(
        &self,
        entry_id: CollectionEntryId,
        settings: SoundSettings,
    ) -> impl Future<Output = ()> + Send;

    /// Routes all audio to `target`, carrying the music at its position and the looping sounds
    /// over. Selecting the active output does nothing.
    fn select_output(
        &self,
        target: OutputTarget,
    ) -> impl Future<Output = Result<(), SelectOutputError>> + Send;

    /// Fades out the music and every sound, then closes the output, leaving any voice channel,
    /// until something plays again.
    fn stand_by(&self) -> impl Future<Output = ()> + Send;

    /// Publishes whether voice channels can be joined, after the Discord connection is disabled
    /// or enabled. Does nothing while a voice channel is the output.
    fn refresh_voice_availability(&self) -> impl Future<Output = ()> + Send;

    /// Plays on the local device whatever the output, pausing music playing there, and ends a
    /// running preview.
    fn start_preview(
        &self,
        preview: SongPreview,
    ) -> impl Future<Output = Result<PreviewSession, PlayAudioError>> + Send;

    /// Resumes music the preview paused. Stopping an ended preview does nothing.
    fn stop_preview(&self, id: PreviewId) -> impl Future<Output = ()> + Send;

    /// Observes the current state and every later change.
    fn watch_player(&self) -> watch::Receiver<PlayerState>;

    fn diagnostics(&self) -> PlayerDiagnostics;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongPreview {
    pub path: AudioFilePath,
    pub draft: LoopRegion,
    pub mode: PreviewMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewId(pub u64);

#[derive(Debug)]
pub struct PreviewSession {
    pub id: PreviewId,
    pub started: PreviewStarted,
    /// Resolves once the preview ends without [`Player::stop_preview`], as when a newer one
    /// replaces it.
    pub ended: oneshot::Receiver<()>,
}

/// Looked up whenever a song starts, so saved changes reach a playing song when it restarts.
pub trait SongLoops: Send + Sync + 'static {
    /// `None` for songs without a saved region and for regions that need review.
    fn loop_region(&self, audio_file_id: AudioFileId) -> Option<LoopRegion>;
}

/// Why an audio file couldn't start playing.
#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum PlayAudioError {
    #[error("the audio file does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error("the audio file cannot be read")]
    #[meta(class = FailedPrecondition, reason = Self::UNREADABLE)]
    Unreadable(#[source] std::io::Error),
    #[error("the file holds no audio track Tufo can decode")]
    #[meta(class = FailedPrecondition, reason = Self::UNSUPPORTED)]
    Unsupported(#[source] anyhow::Error),
    #[error("the active output cannot play audio")]
    #[meta(class = Unavailable, reason = Self::OUTPUT_UNAVAILABLE)]
    OutputUnavailable(#[source] anyhow::Error),
}

impl PlayAudioError {
    pub const NOT_FOUND: &'static str = "playback.player.not_found";
    pub const UNREADABLE: &'static str = "playback.player.unreadable";
    pub const UNSUPPORTED: &'static str = "playback.player.unsupported";
    pub const OUTPUT_UNAVAILABLE: &'static str = "playback.player.output_unavailable";
}
