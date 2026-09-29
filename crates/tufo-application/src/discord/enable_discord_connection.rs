use super::discord_client::DiscordClient;
use crate::playback::player::Player;

pub async fn execute(discord: &impl DiscordClient, player: &impl Player) {
    discord.enable_connection();
    player.refresh_voice_availability().await;
}
