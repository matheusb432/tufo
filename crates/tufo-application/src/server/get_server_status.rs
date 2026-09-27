use std::{num::NonZeroU32, time::Duration};

use tufo_wire::server::{DiscordConnectionStatus, ServerStatus};

use crate::{
    discord::bot_credentials::{BotCredentials, CredentialsError},
    voice_channels::voice_channel_directory::{VoiceChannelDirectory, VoiceChannelDirectoryError},
};

pub async fn execute(
    version: String,
    process_id: NonZeroU32,
    uptime: Duration,
    credentials: &impl BotCredentials,
    channels: &impl VoiceChannelDirectory,
) -> Result<ServerStatus, CredentialsError> {
    let discord_bot_token = credentials.status().await?;
    let discord_connection = match channels.voice_channels() {
        Ok(_) => DiscordConnectionStatus::Ready,
        Err(VoiceChannelDirectoryError::NotConfigured) => DiscordConnectionStatus::NotConfigured,
        Err(VoiceChannelDirectoryError::NotReady) => DiscordConnectionStatus::Connecting,
    };
    Ok(ServerStatus {
        version,
        process_id,
        uptime,
        discord_bot_token,
        discord_connection,
    })
}
