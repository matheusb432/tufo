//! The voice channels the bot can see, kept current from gateway events.

use std::collections::{BTreeMap, BTreeSet};

use tufo_models::discord::{GuildId, VoiceChannel, VoiceChannelId, VoiceChannelTarget};
use twilight_gateway::Event;
use twilight_model::{
    channel::{Channel, ChannelType},
    gateway::payload::incoming::GuildCreate,
    id::{Id, marker::GuildMarker},
};

#[derive(Debug, Default)]
pub(super) struct VoiceChannelCache {
    session: GatewaySession,
    guilds: BTreeMap<GuildId, CachedGuild>,
}

/// How far the gateway session has come in delivering the bot's guilds.
#[derive(Debug, Default)]
enum GatewaySession {
    #[default]
    Connecting,
    /// `READY` named these guilds without their channels. Each arrives next as `GUILD_CREATE`, or
    /// as `GUILD_DELETE` while an outage keeps it unavailable.
    DeliveringGuilds(BTreeSet<GuildId>),
    Ready,
}

#[derive(Debug)]
struct CachedGuild {
    name: String,
    voice_channels: BTreeMap<VoiceChannelId, CachedChannel>,
}

#[derive(Debug)]
struct CachedChannel {
    name: String,
    position: i32,
}

impl VoiceChannelCache {
    pub(super) fn apply(&mut self, event: &Event) {
        match event {
            Event::Ready(ready) => {
                self.start_session(ready.guilds.iter().map(|guild| guild_id(guild.id)));
            }
            Event::GuildCreate(guild_create) => {
                if let GuildCreate::Available(guild) = guild_create.as_ref() {
                    self.insert_guild(guild_id(guild.id), guild.name.clone(), &guild.channels);
                }
                self.guild_delivered(guild_id(guild_create.id()));
            }
            Event::GuildUpdate(update) => {
                if let Some(guild) = self.guilds.get_mut(&guild_id(update.id)) {
                    guild.name.clone_from(&update.name);
                }
            }
            Event::GuildDelete(delete) => {
                self.guilds.remove(&guild_id(delete.id));
                self.guild_delivered(guild_id(delete.id));
            }
            Event::ChannelCreate(channel) => self.upsert_channel(channel),
            Event::ChannelUpdate(channel) => self.upsert_channel(channel),
            Event::ChannelDelete(channel) => self.remove_channel(channel),
            _ => {}
        }
    }

    /// Every visible voice channel, or `None` until the gateway has delivered every guild.
    pub(super) fn voice_channels(&self) -> Option<Vec<VoiceChannel>> {
        matches!(self.session, GatewaySession::Ready).then(|| {
            self.guilds
                .iter()
                .flat_map(|(&guild_id, guild)| {
                    guild
                        .voice_channels
                        .iter()
                        .map(move |(&channel_id, channel)| VoiceChannel {
                            target: VoiceChannelTarget {
                                guild_id,
                                channel_id,
                            },
                            guild_name: guild.name.clone(),
                            channel_name: channel.name.clone(),
                            position: channel.position,
                        })
                })
                .collect()
        })
    }

    pub(super) fn contains(&self, target: VoiceChannelTarget) -> bool {
        self.guilds
            .get(&target.guild_id)
            .is_some_and(|guild| guild.voice_channels.contains_key(&target.channel_id))
    }

    /// Waits for the guilds a first `READY` names. A later `READY`, after the session reconnected,
    /// only refreshes guilds the list already holds.
    fn start_session(&mut self, guild_ids: impl IntoIterator<Item = GuildId>) {
        if matches!(self.session, GatewaySession::Ready) {
            return;
        }
        self.session = GatewaySession::DeliveringGuilds(guild_ids.into_iter().collect());
        self.settle_session();
    }

    fn guild_delivered(&mut self, guild_id: GuildId) {
        if let GatewaySession::DeliveringGuilds(pending) = &mut self.session {
            pending.remove(&guild_id);
            self.settle_session();
        }
    }

    fn settle_session(&mut self) {
        if let GatewaySession::DeliveringGuilds(pending) = &self.session
            && pending.is_empty()
        {
            self.session = GatewaySession::Ready;
        }
    }

    fn insert_guild(&mut self, guild_id: GuildId, name: String, channels: &[Channel]) {
        let voice_channels = channels.iter().filter_map(voice_channel_entry).collect();
        self.guilds.insert(
            guild_id,
            CachedGuild {
                name,
                voice_channels,
            },
        );
    }

    fn upsert_channel(&mut self, channel: &Channel) {
        let Some(guild) = channel
            .guild_id
            .and_then(|id| self.guilds.get_mut(&guild_id(id)))
        else {
            return;
        };
        let channel_id = VoiceChannelId::new(channel.id.into_nonzero());
        match voice_channel_entry(channel) {
            Some((_, cached)) => {
                guild.voice_channels.insert(channel_id, cached);
            }
            None => {
                guild.voice_channels.remove(&channel_id);
            }
        }
    }

    fn remove_channel(&mut self, channel: &Channel) {
        if let Some(guild) = channel
            .guild_id
            .and_then(|id| self.guilds.get_mut(&guild_id(id)))
        {
            guild
                .voice_channels
                .remove(&VoiceChannelId::new(channel.id.into_nonzero()));
        }
    }
}

fn guild_id(id: Id<GuildMarker>) -> GuildId {
    GuildId::new(id.into_nonzero())
}

fn voice_channel_entry(channel: &Channel) -> Option<(VoiceChannelId, CachedChannel)> {
    (channel.kind == ChannelType::GuildVoice).then(|| {
        (
            VoiceChannelId::new(channel.id.into_nonzero()),
            CachedChannel {
                name: channel.name.clone().unwrap_or_default(),
                position: channel.position.unwrap_or_default(),
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use twilight_model::{gateway::payload::incoming::GuildDelete, guild::UnavailableGuild};

    use super::*;

    fn guild(id: u64) -> GuildId {
        GuildId::new(NonZeroU64::new(id).unwrap())
    }

    fn unavailable_guild_create(id: u64) -> Event {
        Event::GuildCreate(Box::new(GuildCreate::Unavailable(UnavailableGuild {
            id: Id::new(id),
            unavailable: true,
        })))
    }

    fn outage_guild_delete(id: u64) -> Event {
        Event::GuildDelete(GuildDelete {
            id: Id::new(id),
            unavailable: Some(true),
        })
    }

    #[test]
    fn lists_nothing_until_every_guild_ready_named_arrived() {
        let mut cache = VoiceChannelCache::default();
        cache.start_session([guild(1), guild(2)]);
        let while_delivering = cache.voice_channels();

        cache.insert_guild(guild(1), "Table".to_owned(), &[]);
        cache.apply(&unavailable_guild_create(1));
        let with_one_pending = cache.voice_channels();
        cache.apply(&outage_guild_delete(2));

        assert!(while_delivering.is_none());
        assert!(with_one_pending.is_none());
        assert_eq!(cache.voice_channels(), Some(Vec::new()));
    }

    #[test]
    fn a_bot_in_no_guild_is_ready_at_once() {
        let mut cache = VoiceChannelCache::default();

        cache.start_session([]);

        assert_eq!(cache.voice_channels(), Some(Vec::new()));
    }

    #[test]
    fn a_reconnected_session_keeps_listing() {
        let mut cache = VoiceChannelCache::default();
        cache.start_session([]);

        cache.start_session([guild(1)]);

        assert!(cache.voice_channels().is_some());
    }
}
