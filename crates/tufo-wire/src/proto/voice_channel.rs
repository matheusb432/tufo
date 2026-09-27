//! Protobuf mappings for listing voice channels.
//!
//! A page token names the last channel of the previous page as `guild_id:channel_id`. Clients
//! must treat it as opaque.

use tonic::Status;
use tufo_models::discord::{VoiceChannel, VoiceChannelTarget};

use super::{InvalidMessage, invalid, voice_channel_target};
use crate::{
    pb,
    voice_channels::{ListVoiceChannels, VoiceChannelPage, VoiceChannelPageSize},
};

pub fn list_voice_channels_request(
    request: &pb::ListVoiceChannelsRequest,
) -> Result<ListVoiceChannels, Status> {
    let after = match request.page_token.as_str() {
        "" => None,
        token => Some(
            page_cursor(token)
                .ok_or_else(|| invalid("page_token", "is not a token this server issued"))?,
        ),
    };
    let page_size = usize::try_from(request.page_size).unwrap_or(usize::MAX);
    Ok(ListVoiceChannels {
        after,
        page_size: VoiceChannelPageSize::from_requested(page_size),
    })
}

/// Requests the largest page after `page_token`, which is empty for the first page.
#[must_use]
pub fn list_voice_channels_message(page_token: String) -> pb::ListVoiceChannelsRequest {
    let page_size = u32::try_from(VoiceChannelPageSize::MAX.get()).unwrap_or(u32::MAX);
    pb::ListVoiceChannelsRequest {
        page_size,
        page_token,
    }
}

#[must_use]
pub fn list_voice_channels_response(page: VoiceChannelPage) -> pb::ListVoiceChannelsResponse {
    pb::ListVoiceChannelsResponse {
        voice_channels: page
            .channels
            .into_iter()
            .map(|channel| pb::VoiceChannel {
                guild_id: channel.target.guild_id.into_inner().get(),
                channel_id: channel.target.channel_id.into_inner().get(),
                guild_name: channel.guild_name,
                channel_name: channel.channel_name,
                position: channel.position,
            })
            .collect(),
        next_page_token: page.next_after.map(page_token).unwrap_or_default(),
    }
}

/// Decodes one page, returning its channels and the token of the next page, if any.
pub fn voice_channel_page(
    response: pb::ListVoiceChannelsResponse,
) -> Result<(Vec<VoiceChannel>, Option<String>), InvalidMessage> {
    let channels = response
        .voice_channels
        .into_iter()
        .map(|channel| {
            Ok(VoiceChannel {
                target: voice_channel_target(channel.guild_id, channel.channel_id).map_err(
                    |_| InvalidMessage {
                        field: "voice channel",
                    },
                )?,
                guild_name: channel.guild_name,
                channel_name: channel.channel_name,
                position: channel.position,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let next_page_token =
        (!response.next_page_token.is_empty()).then_some(response.next_page_token);
    Ok((channels, next_page_token))
}

fn page_token(after: VoiceChannelTarget) -> String {
    format!(
        "{}:{}",
        after.guild_id.into_inner(),
        after.channel_id.into_inner()
    )
}

fn page_cursor(token: &str) -> Option<VoiceChannelTarget> {
    let (guild_id, channel_id) = token.split_once(':')?;
    voice_channel_target(guild_id.parse().ok()?, channel_id.parse().ok()?).ok()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use tonic::Code;
    use tufo_models::discord::{GuildId, VoiceChannelId};

    use super::*;

    #[test]
    fn page_tokens_resume_after_the_last_channel() {
        let after = VoiceChannelTarget {
            guild_id: GuildId::new(NonZeroU64::new(959_535_519_157_612_664).unwrap()),
            channel_id: VoiceChannelId::new(NonZeroU64::new(7).unwrap()),
        };
        let response = list_voice_channels_response(VoiceChannelPage {
            channels: Vec::new(),
            next_after: Some(after),
        });

        let request = list_voice_channels_request(&pb::ListVoiceChannelsRequest {
            page_size: 0,
            page_token: response.next_page_token,
        })
        .unwrap();

        assert_eq!(request.after, Some(after));
        assert_eq!(request.page_size, VoiceChannelPageSize::DEFAULT);
    }

    #[test]
    fn foreign_page_tokens_are_invalid_arguments() {
        for page_token in ["garbage", "1:0", "1:2:3"] {
            let result = list_voice_channels_request(&pb::ListVoiceChannelsRequest {
                page_size: 10,
                page_token: page_token.to_owned(),
            });

            assert_eq!(
                result.unwrap_err().code(),
                Code::InvalidArgument,
                "{page_token}"
            );
        }
    }
}
