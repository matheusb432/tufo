use std::num::NonZeroU32;

use super::{InvalidMessage, duration_value};
use crate::{
    pb,
    server::{DiscordBotTokenStatus, DiscordConnectionStatus, ServerStatus},
};

pub fn server_status(message: pb::GetServerStatusResponse) -> Result<ServerStatus, InvalidMessage> {
    let invalid = || InvalidMessage {
        field: "server status",
    };
    if message.version.trim().is_empty() {
        return Err(invalid());
    }
    let process_id = NonZeroU32::new(message.process_id).ok_or_else(invalid)?;
    let uptime = message
        .uptime
        .and_then(duration_value)
        .ok_or_else(invalid)?;
    let discord_bot_token = match pb::DiscordBotTokenStatus::try_from(message.discord_bot_token) {
        Ok(pb::DiscordBotTokenStatus::Missing) => DiscordBotTokenStatus::Missing,
        Ok(pb::DiscordBotTokenStatus::Configured) => DiscordBotTokenStatus::Configured,
        Ok(pb::DiscordBotTokenStatus::Invalid) => DiscordBotTokenStatus::Invalid,
        _ => return Err(invalid()),
    };
    let discord_connection = match pb::DiscordConnectionStatus::try_from(message.discord_connection)
    {
        Ok(pb::DiscordConnectionStatus::NotConfigured) => DiscordConnectionStatus::NotConfigured,
        Ok(pb::DiscordConnectionStatus::Connecting) => DiscordConnectionStatus::Connecting,
        Ok(pb::DiscordConnectionStatus::Ready) => DiscordConnectionStatus::Ready,
        _ => return Err(invalid()),
    };
    Ok(ServerStatus {
        version: message.version,
        process_id,
        uptime,
        discord_bot_token,
        discord_connection,
    })
}

#[must_use]
pub fn server_status_message(status: ServerStatus) -> pb::GetServerStatusResponse {
    pb::GetServerStatusResponse {
        version: status.version,
        process_id: status.process_id.get(),
        uptime: Some(super::duration_message(status.uptime)),
        discord_bot_token: match status.discord_bot_token {
            DiscordBotTokenStatus::Missing => pb::DiscordBotTokenStatus::Missing,
            DiscordBotTokenStatus::Configured => pb::DiscordBotTokenStatus::Configured,
            DiscordBotTokenStatus::Invalid => pb::DiscordBotTokenStatus::Invalid,
        }
        .into(),
        discord_connection: match status.discord_connection {
            DiscordConnectionStatus::NotConfigured => pb::DiscordConnectionStatus::NotConfigured,
            DiscordConnectionStatus::Connecting => pb::DiscordConnectionStatus::Connecting,
            DiscordConnectionStatus::Ready => pb::DiscordConnectionStatus::Ready,
        }
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_incomplete_or_unknown_status_fields() {
        let valid = pb::GetServerStatusResponse {
            version: "0.1.0".into(),
            process_id: 42,
            uptime: Some(prost_types::Duration {
                seconds: 3,
                nanos: 0,
            }),
            discord_bot_token: pb::DiscordBotTokenStatus::Configured.into(),
            discord_connection: pb::DiscordConnectionStatus::Ready.into(),
        };
        assert!(server_status(valid.clone()).is_ok());
        for invalid in [
            pb::GetServerStatusResponse {
                version: String::new(),
                ..valid.clone()
            },
            pb::GetServerStatusResponse {
                process_id: 0,
                ..valid.clone()
            },
            pb::GetServerStatusResponse {
                uptime: None,
                ..valid.clone()
            },
            pb::GetServerStatusResponse {
                uptime: Some(prost_types::Duration {
                    seconds: -1,
                    nanos: 0,
                }),
                ..valid.clone()
            },
            pb::GetServerStatusResponse {
                discord_bot_token: 0,
                ..valid.clone()
            },
            pb::GetServerStatusResponse {
                discord_connection: 99,
                ..valid
            },
        ] {
            assert!(server_status(invalid).is_err());
        }
    }
}
