use tufo_models::playback::diagnostics::PlayerDiagnostics;

use super::player::Player;

/// Reports whether playback kept its timing since the server started.
pub fn execute(player: &impl Player) -> PlayerDiagnostics {
    player.diagnostics()
}
