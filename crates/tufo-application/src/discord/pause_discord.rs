use tufo_models::playback::OutputTarget;

use super::discord_gateway::DiscordGateway;
use crate::playback::player::Player;

/// Leaves any voice channel, then stops the bot's gateway connection, so Discord uses no network
/// and can't be joined by accident until resumed.
pub async fn execute(gateway: &impl DiscordGateway, player: &impl Player) {
    let _ = player.select_output(OutputTarget::Local).await;
    gateway.pause();
    player.refresh_voice_availability().await;
}
