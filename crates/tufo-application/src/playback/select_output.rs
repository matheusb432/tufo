use tufo_models::playback::{
    OutputTarget,
    local_output::{LocalOutputFailure, LocalOutputFailureReason, LocalOutputIdentity},
};

use super::player::Player;
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum SelectOutputError {
    #[error("Discord is not configured on this server")]
    #[meta(class = FailedPrecondition, reason = Self::DISCORD_NOT_CONFIGURED)]
    DiscordNotConfigured,
    #[error("the Discord connection is disabled on this server")]
    #[meta(class = FailedPrecondition, reason = Self::DISCORD_DISABLED)]
    DiscordDisabled,
    #[error("the bot cannot see that voice channel")]
    #[meta(class = NotFound, reason = Self::UNKNOWN_VOICE_CHANNEL)]
    UnknownVoiceChannel,
    #[error("joining the voice channel failed")]
    #[meta(class = Unavailable, reason = Self::VOICE_CONNECTION)]
    VoiceConnection(#[source] anyhow::Error),
    #[error("the local audio device cannot play")]
    #[meta(class = Unavailable, reason = Self::LOCAL_DEVICE)]
    LocalDevice {
        reason: LocalOutputFailureReason,
        identity: Option<LocalOutputIdentity>,
        #[source]
        source: anyhow::Error,
    },
}

impl SelectOutputError {
    #[must_use]
    pub fn local_failure(&self) -> Option<LocalOutputFailure> {
        match self {
            Self::LocalDevice {
                reason,
                identity,
                source,
            } => Some(LocalOutputFailure {
                reason: *reason,
                identity: identity.clone(),
                detail: format!("{source:#}"),
            }),
            _ => None,
        }
    }

    pub const DISCORD_NOT_CONFIGURED: &'static str =
        "playback.select_output.discord_not_configured";
    pub const DISCORD_DISABLED: &'static str = "playback.select_output.discord_disabled";
    pub const UNKNOWN_VOICE_CHANNEL: &'static str = "playback.select_output.unknown_voice_channel";
    pub const VOICE_CONNECTION: &'static str = "playback.select_output.voice_connection";
    pub const LOCAL_DEVICE: &'static str = "playback.select_output.local_device";
}

/// Routes playback to the local device or one Discord voice channel, carrying the music and the
/// looping sounds over. Selecting the active output again does nothing.
pub async fn execute(target: OutputTarget, player: &impl Player) -> Result<(), SelectOutputError> {
    player.select_output(target).await
}
