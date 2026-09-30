use tufo_models::playback::queue::{NewQueueError, QueueStart};

use super::player::{PlayAudioError, Player};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum PlayQueueError {
    #[error(transparent)]
    #[meta(class = Internal, reason = Self::INVALID_QUEUE)]
    InvalidQueue(#[from] NewQueueError),
    #[error(transparent)]
    #[meta(transparent)]
    Audio(#[from] PlayAudioError),
}

impl PlayQueueError {
    pub const INVALID_QUEUE: &'static str = "playback.play_queue.invalid_queue";
}

/// Plays a collection's tracks from `start`, replacing the current music.
pub async fn execute(start: QueueStart, player: &impl Player) -> Result<(), PlayQueueError> {
    player.play_queue(start).await
}
