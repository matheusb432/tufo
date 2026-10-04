use std::{num::NonZeroU32, time::Duration};

use tufo_wire::server::{DiscordConnectionStatus, ServerStatus};

use crate::discord::{
    bot_credentials::{BotCredentials, CredentialsError},
    discord_client::{DiscordClient, DiscordClientError},
};

pub async fn execute(
    version: String,
    process_id: NonZeroU32,
    uptime: Duration,
    credentials: &impl BotCredentials,
    discord: &impl DiscordClient,
) -> Result<ServerStatus, CredentialsError> {
    let discord_bot_token = credentials.status().await?;
    let discord_connection = match discord.voice_channels() {
        Ok(_) => DiscordConnectionStatus::Ready,
        Err(DiscordClientError::NotConfigured) => DiscordConnectionStatus::NotConfigured,
        Err(DiscordClientError::Disabled) => DiscordConnectionStatus::Disabled,
        Err(DiscordClientError::NotReady) => DiscordConnectionStatus::Connecting,
    };
    Ok(ServerStatus {
        version,
        process_id,
        uptime,
        discord_bot_token,
        discord_connection,
        supports_library_patches: true,
    })
}
