//! Read-only process and Discord status. Credentials never cross this boundary.

use std::{num::NonZeroU32, time::Duration};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStatus {
    pub version: String,
    pub process_id: NonZeroU32,
    pub uptime: Duration,
    pub discord_bot_token: DiscordBotTokenStatus,
    pub discord_connection: DiscordConnectionStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordBotTokenStatus {
    Missing,
    Configured,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordConnectionStatus {
    NotConfigured,
    Connecting,
    Ready,
    Paused,
}
