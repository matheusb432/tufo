use super::player::Player;

/// Saves the browser's tabs and ends it with every page process, releasing their memory.
/// Closing a closed browser is harmless.
pub async fn execute(player: &impl Player) {
    player.close_browser().await;
}
