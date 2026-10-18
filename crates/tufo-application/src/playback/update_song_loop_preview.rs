//! Moving a running preview onto a newer draft while it plays.

use tufo_wire::song_loops::UpdateSongLoopPreview;

use super::player::Player;
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum UpdateSongLoopPreviewError {
    #[error("the preview ended")]
    #[meta(class = NotFound, reason = Self::ENDED)]
    Ended,
    #[error("a seam preview keeps the draft it started with")]
    #[meta(class = FailedPrecondition, reason = Self::SEAM)]
    Seam,
    #[error("the draft is for other audio than the preview plays")]
    #[meta(class = FailedPrecondition, reason = Self::OTHER_AUDIO)]
    OtherAudio,
}

impl UpdateSongLoopPreviewError {
    pub const ENDED: &'static str = "playback.update_song_loop_preview.ended";
    pub const SEAM: &'static str = "playback.update_song_loop_preview.seam";
    pub const OTHER_AUDIO: &'static str = "playback.update_song_loop_preview.other_audio";
}

/// Returns once the preview holds the draft; its session reports when the draft plays.
pub async fn execute(
    request: UpdateSongLoopPreview,
    player: &impl Player,
) -> Result<(), UpdateSongLoopPreviewError> {
    player
        .update_preview(request.preview_id, request.draft)
        .await
}
