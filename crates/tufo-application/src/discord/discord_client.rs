//! The capability that reaches Discord through the server's bot.

use tufo_models::discord::VoiceChannel;

use crate::error::ErrorMeta;

/// The server's Discord bot, whose gateway connection can be disabled and enabled at runtime.
pub trait DiscordClient: Send + Sync {
    /// Returns every voice channel the bot can see, in no particular order.
    fn voice_channels(&self) -> Result<Vec<VoiceChannel>, DiscordClientError>;

    /// Logs the bot out of the gateway, so it can't join a voice channel and uses no network.
    /// Leave any voice channel first. Does nothing when not configured or already disabled.
    fn disable_connection(&self);

    /// Logs the bot back into the gateway. Does nothing when not configured or already enabled.
    fn enable_connection(&self);
}

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum DiscordClientError {
    #[error("Discord is not configured on this server")]
    #[meta(class = FailedPrecondition, reason = Self::NOT_CONFIGURED)]
    NotConfigured,
    #[error("the Discord connection is disabled on this server")]
    #[meta(class = FailedPrecondition, reason = Self::DISABLED)]
    Disabled,
    #[error("the Discord gateway has not delivered the bot's guilds yet")]
    #[meta(class = Unavailable, reason = Self::NOT_READY)]
    NotReady,
}

impl DiscordClientError {
    pub const NOT_CONFIGURED: &'static str = "discord.discord_client.not_configured";
    pub const DISABLED: &'static str = "discord.discord_client.disabled";
    pub const NOT_READY: &'static str = "discord.discord_client.not_ready";
}
