//! Starting the browser whose pages play through the mixer.

use super::player::Player;
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum OpenBrowserError {
    #[error("this installation has no browser")]
    #[meta(class = Unavailable, reason = Self::UNAVAILABLE)]
    Unavailable,
    #[error("the browser cannot start")]
    #[meta(class = Unavailable, reason = Self::START_FAILED)]
    StartFailed(#[source] anyhow::Error),
}

impl OpenBrowserError {
    pub const UNAVAILABLE: &'static str = "playback.open_browser.unavailable";
    pub const START_FAILED: &'static str = "playback.open_browser.start_failed";
}

/// Starts the browser unless it is starting or open; its status turns open once it accepts
/// views.
pub async fn execute(player: &impl Player) -> Result<(), OpenBrowserError> {
    player.open_browser().await
}
