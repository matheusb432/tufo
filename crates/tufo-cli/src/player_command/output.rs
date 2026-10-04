use std::{num::NonZeroU64, str::FromStr};

use clap::Subcommand;
use serde::Deserialize;
use tufo_models::{
    discord::{GuildId, VoiceChannelId, VoiceChannelTarget},
    playback::OutputTarget,
};

#[derive(Debug, Subcommand)]
pub(crate) enum OutputCommand {
    /// Read the current local or Discord output target.
    Get,
    /// Select the local device or one Discord voice channel.
    #[command(after_long_help = r#"Examples:
  tufo output select '{"kind":"local"}'
  tufo output select '{"kind":"discord_voice","guild_id":"123456789012345678","channel_id":"234567890123456789"}'

Discord IDs are nonzero decimal strings. Discover them with tufo discord channels.
Selecting local output replaces any Discord channel; no device name is required."#)]
    Select {
        /// One JSON object describing the complete output target.
        #[arg(value_name = "JSON_PAYLOAD")]
        payload: OutputPayload,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "OutputJson")]
pub(crate) struct OutputPayload(OutputTarget);

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OutputJson {
    Local {},
    DiscordVoice {
        guild_id: String,
        channel_id: String,
    },
}

impl TryFrom<OutputJson> for OutputPayload {
    type Error = String;

    fn try_from(json: OutputJson) -> Result<Self, Self::Error> {
        let output = match json {
            OutputJson::Local {} => OutputTarget::Local,
            OutputJson::DiscordVoice {
                guild_id,
                channel_id,
            } => OutputTarget::DiscordVoice(VoiceChannelTarget {
                guild_id: GuildId::new(parse_snowflake(&guild_id, "guild_id")?),
                channel_id: VoiceChannelId::new(parse_snowflake(&channel_id, "channel_id")?),
            }),
        };
        Ok(Self(output))
    }
}

impl FromStr for OutputPayload {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        crate::input::parse_json(text)
    }
}

fn parse_snowflake(text: &str, field: &str) -> Result<NonZeroU64, String> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("{field} must be a nonzero decimal string"));
    }
    text.parse()
        .map_err(|_| format!("{field} must be a nonzero decimal string fitting in u64"))
}

pub(crate) async fn run_output(command: OutputCommand, json: bool) -> anyhow::Result<bool> {
    let client = crate::api::connect().await?;
    match command {
        OutputCommand::Get => {
            let state = crate::api::player_state(&client).await?;
            crate::api::emit(&crate::record::output(state.output), json).await?;
        }
        OutputCommand::Select { payload } => {
            let id = match payload.0 {
                OutputTarget::Local => "local".to_owned(),
                OutputTarget::DiscordVoice(target) => format!(
                    "{}:{}",
                    target.guild_id.into_inner(),
                    target.channel_id.into_inner(),
                ),
            };
            client.select_output(payload.0).await?;
            crate::api::ack("select", "output", id, json).await?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_json_round_trips_full_snowflakes() -> Result<(), String> {
        let payload: OutputPayload =
            r#"{"kind":"discord_voice","guild_id":"18446744073709551615","channel_id":"1"}"#
                .parse()?;
        let output = crate::record::output(payload.0);

        assert_eq!(output["kind"], "discord_voice");
        assert_eq!(output["guild_id"], u64::MAX.to_string());
        assert_eq!(output["channel_id"], "1");
        assert_eq!(
            r#"{"kind":"local"}"#.parse::<OutputPayload>()?.0,
            OutputTarget::Local
        );
        Ok(())
    }

    #[test]
    fn output_rejects_unknown_fields_and_malformed_targets() {
        for text in [
            r#"{"kind":"local","extra":true}"#,
            r#"{"kind":"local","guild_id":"1"}"#,
            r#"{"kind":"discord_voice","guild_id":"0","channel_id":"1"}"#,
            r#"{"kind":"discord_voice","guild_id":"1","channel_id":"18446744073709551616"}"#,
            r#"{"kind":"discord_voice","guild_id":"+1","channel_id":"2"}"#,
            r#"{"kind":"discord_voice","guild_id":1,"channel_id":"2"}"#,
            r#"{"kind":"discord_voice","guild_id":"1"}"#,
            r#"{"kind":"discord_voice","guild_id":"1","channel_id":"2","extra":true}"#,
            r#"{"kind":"discord_voice","guild_id":{"id":"1","extra":true},"channel_id":"2"}"#,
            r#"{"kind":"other"}"#,
        ] {
            assert!(text.parse::<OutputPayload>().is_err(), "accepted {text}");
        }
    }
}
