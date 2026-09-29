use tufo_models::playback::OutputTarget;

use super::discord_client::DiscordClient;
use crate::playback::player::Player;

/// Moves playback off any voice channel, then logs the bot out until the connection is enabled.
pub async fn execute(discord: &impl DiscordClient, player: &impl Player) {
    if let Err(error) = player.select_output(OutputTarget::Local).await {
        tracing::warn!(%error, "cannot move playback to the local device");
    }
    discord.disable_connection();
    player.refresh_voice_availability().await;
}
