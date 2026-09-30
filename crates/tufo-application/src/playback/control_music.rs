use std::time::Duration;

use tufo_models::{
    library::Volume,
    playback::{player_state::CrossfadeDuration, queue::RepeatMode},
};

use super::player::{PlayAudioError, Player};
use crate::error::ErrorMeta;

/// One change to the music. Settings apply with or without a queue; the rest need one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MusicCommand {
    Pause,
    Resume,
    /// Fades the music out and drops the queue.
    Stop,
    Next,
    /// Goes back a track, or restarts the current one once it has played for a few seconds.
    Previous,
    Seek(Duration),
    SetVolume(Volume),
    SetMuted(bool),
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    SetCrossfade(CrossfadeDuration),
}

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum ControlMusicError {
    #[error("no music is playing")]
    #[meta(class = FailedPrecondition, reason = Self::NOTHING_PLAYING)]
    NothingPlaying,
    #[error("the position is past the end of the track")]
    #[meta(class = FailedPrecondition, reason = Self::SEEK_PAST_END)]
    SeekPastEnd,
    #[error(transparent)]
    #[meta(transparent)]
    Audio(#[from] PlayAudioError),
}

impl ControlMusicError {
    pub const NOTHING_PLAYING: &'static str = "playback.control_music.nothing_playing";
    pub const SEEK_PAST_END: &'static str = "playback.control_music.seek_past_end";
}

/// Applies `command` to the music. Stopping while nothing plays succeeds.
pub async fn execute(command: MusicCommand, player: &impl Player) -> Result<(), ControlMusicError> {
    player.control_music(command).await
}
