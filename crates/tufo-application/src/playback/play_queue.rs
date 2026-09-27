use tufo_models::playback::queue::{NewQueueError, QueueStart};

use super::player::{PlayAudioError, Player};
use crate::error::{ApplicationError, ErrorClass, ErrorMetadata};

#[derive(Debug, thiserror::Error)]
pub enum PlayQueueError {
    #[error(transparent)]
    InvalidQueue(#[from] NewQueueError),
    #[error(transparent)]
    Audio(#[from] PlayAudioError),
}

impl PlayQueueError {
    pub const INVALID_QUEUE: &'static str = "playback.play_queue.invalid_queue";
}

impl ApplicationError for PlayQueueError {
    fn metadata(&self) -> ErrorMetadata {
        match self {
            Self::InvalidQueue(_) => ErrorMetadata {
                class: ErrorClass::Internal,
                reason: Self::INVALID_QUEUE,
            },
            Self::Audio(error) => error.metadata(),
        }
    }
}

/// Plays a collection's tracks from `start`, replacing the current music.
pub async fn execute(start: QueueStart, player: &impl Player) -> Result<(), PlayQueueError> {
    player.play_queue(start).await
}
