//! The capability that knows which voice channels the bot can see.

use tufo_models::discord::VoiceChannel;

use crate::error::{ApplicationError, ErrorClass, ErrorMetadata};

/// The voice channels the Discord bot can currently see.
pub trait VoiceChannelDirectory: Send + Sync {
    /// Returns every visible voice channel in no particular order.
    fn voice_channels(&self) -> Result<Vec<VoiceChannel>, VoiceChannelDirectoryError>;
}

#[derive(Debug, thiserror::Error)]
pub enum VoiceChannelDirectoryError {
    #[error("Discord is not configured on this server")]
    NotConfigured,
    #[error("Discord is paused on this server")]
    Paused,
    #[error("the Discord gateway has not delivered the bot's guilds yet")]
    NotReady,
}

impl VoiceChannelDirectoryError {
    pub const NOT_CONFIGURED: &'static str =
        "voice_channels.voice_channel_directory.not_configured";
    pub const PAUSED: &'static str = "voice_channels.voice_channel_directory.paused";
    pub const NOT_READY: &'static str = "voice_channels.voice_channel_directory.not_ready";
}

impl ApplicationError for VoiceChannelDirectoryError {
    fn metadata(&self) -> ErrorMetadata {
        match self {
            Self::NotConfigured => ErrorMetadata {
                class: ErrorClass::FailedPrecondition,
                reason: Self::NOT_CONFIGURED,
            },
            Self::Paused => ErrorMetadata {
                class: ErrorClass::FailedPrecondition,
                reason: Self::PAUSED,
            },
            Self::NotReady => ErrorMetadata {
                class: ErrorClass::Unavailable,
                reason: Self::NOT_READY,
            },
        }
    }
}
