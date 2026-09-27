//! The audio engine capability the playback interactors drive.

use std::future::Future;

use tokio::sync::watch;
use tufo_models::{
    library::{CollectionEntryId, SoundSettings},
    playback::{
        OutputTarget,
        diagnostics::PlayerDiagnostics,
        player_state::{PlayerState, SoundCue},
        queue::QueueStart,
    },
};

use super::{
    control_music::{ControlMusicError, MusicCommand},
    control_sounds::SoundsCommand,
    play_queue::PlayQueueError,
    select_output::SelectOutputError,
};
use crate::error::{ApplicationError, ErrorClass, ErrorMetadata};

/// Plays one music queue and any number of sound effects on exactly one output, and publishes
/// what it plays.
pub trait Player: Send + Sync {
    /// Replaces the music with a queue of `start`'s tracks, applying the player's shuffle and
    /// repeat modes, and plays its start track.
    fn play_queue(
        &self,
        start: QueueStart,
    ) -> impl Future<Output = Result<(), PlayQueueError>> + Send;

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

    /// Observes the current state and every later change.
    fn watch_player(&self) -> watch::Receiver<PlayerState>;

    fn diagnostics(&self) -> PlayerDiagnostics;
}

/// Why an audio file couldn't start playing.
#[derive(Debug, thiserror::Error)]
pub enum PlayAudioError {
    #[error("the audio file does not exist")]
    NotFound,
    #[error("the audio file cannot be read")]
    Unreadable(#[source] std::io::Error),
    #[error("the file holds no audio track Tufo can decode")]
    Unsupported(#[source] anyhow::Error),
    #[error("the active output cannot play audio")]
    OutputUnavailable(#[source] anyhow::Error),
}

impl PlayAudioError {
    pub const NOT_FOUND: &'static str = "playback.player.not_found";
    pub const UNREADABLE: &'static str = "playback.player.unreadable";
    pub const UNSUPPORTED: &'static str = "playback.player.unsupported";
    pub const OUTPUT_UNAVAILABLE: &'static str = "playback.player.output_unavailable";
}

impl ApplicationError for PlayAudioError {
    fn metadata(&self) -> ErrorMetadata {
        match self {
            Self::NotFound => ErrorMetadata {
                class: ErrorClass::NotFound,
                reason: Self::NOT_FOUND,
            },
            Self::Unreadable(_) => ErrorMetadata {
                class: ErrorClass::FailedPrecondition,
                reason: Self::UNREADABLE,
            },
            Self::Unsupported(_) => ErrorMetadata {
                class: ErrorClass::FailedPrecondition,
                reason: Self::UNSUPPORTED,
            },
            Self::OutputUnavailable(_) => ErrorMetadata {
                class: ErrorClass::Unavailable,
                reason: Self::OUTPUT_UNAVAILABLE,
            },
        }
    }
}
