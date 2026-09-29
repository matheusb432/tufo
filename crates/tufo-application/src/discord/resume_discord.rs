use super::discord_gateway::DiscordGateway;
use crate::playback::player::Player;

/// Logs the bot back into the gateway after a pause.
pub async fn execute(gateway: &impl DiscordGateway, player: &impl Player) {
    gateway.resume();
    player.refresh_voice_availability().await;
}
