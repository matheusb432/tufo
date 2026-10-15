use super::player::Player;
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum RecoverLocalOutputError {
    #[error("select local output before recovering it")]
    #[meta(class = FailedPrecondition, reason = Self::NOT_LOCAL)]
    NotLocal,
}

impl RecoverLocalOutputError {
    pub const NOT_LOCAL: &'static str = "playback.recover_local_output.not_local";
}

/// Starts one recovery attempt. Concurrent retries and an already usable output are no-ops.
/// The watch stream reports progress and the eventual result, even if this caller disconnects.
pub async fn execute(player: &impl Player) -> Result<(), RecoverLocalOutputError> {
    player.recover_local_output().await
}
