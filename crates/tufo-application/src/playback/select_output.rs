use tufo_models::playback::OutputTarget;

use super::player::Player;
use crate::error::{ApplicationError, ErrorClass, ErrorMetadata};

#[derive(Debug, thiserror::Error)]
pub enum SelectOutputError {
    #[error("Discord is not configured on this server")]
    DiscordNotConfigured,
    #[error("the Discord connection is disabled on this server")]
    DiscordDisabled,
    #[error("the bot cannot see that voice channel")]
    UnknownVoiceChannel,
    #[error("joining the voice channel failed")]
    VoiceConnection(#[source] anyhow::Error),
    #[error("the local audio device cannot play")]
    LocalDevice(#[source] anyhow::Error),
}

impl SelectOutputError {
    pub const DISCORD_NOT_CONFIGURED: &'static str =
        "playback.select_output.discord_not_configured";
    pub const DISCORD_DISABLED: &'static str = "playback.select_output.discord_disabled";
    pub const UNKNOWN_VOICE_CHANNEL: &'static str = "playback.select_output.unknown_voice_channel";
    pub const VOICE_CONNECTION: &'static str = "playback.select_output.voice_connection";
    pub const LOCAL_DEVICE: &'static str = "playback.select_output.local_device";
}

impl ApplicationError for SelectOutputError {
    fn metadata(&self) -> ErrorMetadata {
        match self {
            Self::DiscordNotConfigured => ErrorMetadata {
                class: ErrorClass::FailedPrecondition,
                reason: Self::DISCORD_NOT_CONFIGURED,
            },
            Self::DiscordDisabled => ErrorMetadata {
                class: ErrorClass::FailedPrecondition,
                reason: Self::DISCORD_DISABLED,
            },
            Self::UnknownVoiceChannel => ErrorMetadata {
                class: ErrorClass::NotFound,
                reason: Self::UNKNOWN_VOICE_CHANNEL,
            },
            Self::VoiceConnection(_) => ErrorMetadata {
                class: ErrorClass::Unavailable,
                reason: Self::VOICE_CONNECTION,
            },
            Self::LocalDevice(_) => ErrorMetadata {
                class: ErrorClass::Unavailable,
                reason: Self::LOCAL_DEVICE,
            },
        }
    }
}

/// Routes playback to the local device or one Discord voice channel, carrying the music and the
/// looping sounds over. Selecting the active output again does nothing.
pub async fn execute(target: OutputTarget, player: &impl Player) -> Result<(), SelectOutputError> {
    player.select_output(target).await
}
