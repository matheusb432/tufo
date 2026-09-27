use tokio::sync::watch;
use tufo_models::playback::player_state::PlayerState;

use super::player::Player;

/// Observes the current player state, then each change. A slow observer skips intermediate
/// states and sees only the latest.
pub fn execute(player: &impl Player) -> watch::Receiver<PlayerState> {
    player.watch_player()
}
