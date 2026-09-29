use tufo_wire::voice_channels::{ListVoiceChannels, VoiceChannelPage};

use crate::discord::discord_client::{DiscordClient, DiscordClientError};

/// Lists the voice channels the bot can see, one page at a time.
pub fn execute(
    request: &ListVoiceChannels,
    discord: &impl DiscordClient,
) -> Result<VoiceChannelPage, DiscordClientError> {
    let mut channels = discord.voice_channels()?;
    channels.sort_unstable_by_key(|channel| channel.target);
    let mut remaining = channels
        .into_iter()
        .filter(|channel| request.after.is_none_or(|after| channel.target > after))
        .peekable();
    let page = remaining
        .by_ref()
        .take(request.page_size.get())
        .collect::<Vec<_>>();
    let next_after = match remaining.peek() {
        Some(_) => page.last().map(|channel| channel.target),
        None => None,
    };
    Ok(VoiceChannelPage {
        channels: page,
        next_after,
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use tufo_models::discord::{GuildId, VoiceChannel, VoiceChannelId, VoiceChannelTarget};
    use tufo_wire::voice_channels::VoiceChannelPageSize;

    use super::*;

    struct FixedChannels(Vec<VoiceChannel>);

    impl DiscordClient for FixedChannels {
        fn voice_channels(&self) -> Result<Vec<VoiceChannel>, DiscordClientError> {
            Ok(self.0.clone())
        }

        fn disable_connection(&self) {}

        fn enable_connection(&self) {}
    }

    fn channel(guild_id: u64, channel_id: u64) -> VoiceChannel {
        VoiceChannel {
            target: VoiceChannelTarget {
                guild_id: GuildId::new(NonZeroU64::new(guild_id).unwrap()),
                channel_id: VoiceChannelId::new(NonZeroU64::new(channel_id).unwrap()),
            },
            guild_name: format!("guild {guild_id}"),
            channel_name: format!("channel {channel_id}"),
            position: 0,
        }
    }

    fn channel_ids(page: &VoiceChannelPage) -> Vec<u64> {
        page.channels
            .iter()
            .map(|channel| channel.target.channel_id.into_inner().get())
            .collect()
    }

    #[test]
    fn pages_follow_guild_then_channel_order_until_the_last_page() {
        let discord = FixedChannels(vec![channel(2, 5), channel(1, 9), channel(1, 3)]);
        let page_size = VoiceChannelPageSize::from_requested(2);

        let first = execute(
            &ListVoiceChannels {
                after: None,
                page_size,
            },
            &discord,
        )
        .unwrap();
        let second = execute(
            &ListVoiceChannels {
                after: first.next_after,
                page_size,
            },
            &discord,
        )
        .unwrap();

        assert_eq!(channel_ids(&first), [3, 9]);
        assert_eq!(channel_ids(&second), [5]);
        assert_eq!(second.next_after, None);
    }

    #[test]
    fn an_exactly_full_last_page_has_no_cursor() {
        let discord = FixedChannels(vec![channel(1, 1), channel(1, 2)]);

        let page = execute(
            &ListVoiceChannels {
                after: None,
                page_size: VoiceChannelPageSize::from_requested(2),
            },
            &discord,
        )
        .unwrap();

        assert_eq!(channel_ids(&page), [1, 2]);
        assert_eq!(page.next_after, None);
    }
}
