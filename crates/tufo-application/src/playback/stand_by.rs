use super::player::Player;

/// Silences the player and lets go of its output, for a server that outlives Tufo's window.
/// Standing by twice is harmless.
pub async fn execute(player: &impl Player) {
    player.stand_by().await;
}
