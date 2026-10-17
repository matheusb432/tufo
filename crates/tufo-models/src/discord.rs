//! Discord identities addressed when streaming into voice channels.

use std::num::NonZeroU64;

use nutype::nutype;

/// A Discord guild (server) snowflake.
#[nutype(derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash))]
pub struct GuildId(NonZeroU64);

/// A Discord voice channel snowflake.
#[nutype(derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash))]
pub struct VoiceChannelId(NonZeroU64);

/// One voice channel of one guild, as the bot joins it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VoiceChannelTarget {
    pub guild_id: GuildId,
    pub channel_id: VoiceChannelId,
}

/// A voice channel the bot can see, with the names people know it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceChannel {
    pub target: VoiceChannelTarget,
    pub guild_name: String,
    pub channel_name: String,
    /// The channel's sorting position inside its guild's channel list.
    pub position: i32,
}
