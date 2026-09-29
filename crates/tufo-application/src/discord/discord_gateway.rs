//! The capability that starts and stops the bot's gateway connection.

/// Whether the bot's gateway connection to Discord is running.
pub trait DiscordGateway: Send + Sync {
    /// Stops the gateway connection. Does nothing when not configured or already paused.
    fn pause(&self);

    /// Logs the bot back into the gateway. Does nothing when not configured or not paused.
    fn resume(&self);
}
