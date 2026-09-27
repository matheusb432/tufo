use tufo_models::playback::player_state::SoundCue;

use super::player::{PlayAudioError, Player};

/// Fires a sound effect over the music. Firing a playing entry again restarts it.
pub async fn execute(cue: SoundCue, player: &impl Player) -> Result<(), PlayAudioError> {
    player.play_sound(cue).await
}
