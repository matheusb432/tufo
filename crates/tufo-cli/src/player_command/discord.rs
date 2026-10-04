use std::{fmt, fs::File, io::Read as _, path::PathBuf, time::Duration};

use anyhow::{Context as _, anyhow};
use clap::Subcommand;
use serde_json::json;
use tufo_client::server::{DiscordBotTokenStatus, DiscordConnectionStatus};

const TOKEN_BYTES_MAX: usize = 4096;
const TOKEN_INPUT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Subcommand)]
pub(crate) enum DiscordCommand {
    /// Inspect credential and Discord connection status without revealing the token.
    Status,
    /// List every Discord voice channel visible to the bot.
    Channels,
    /// Store a bot token from stdin or a file; apply it with tufo server restart.
    #[command(after_long_help = r#"Examples:
  tufo discord token --file /path/to/private-token.txt
  tufo discord token --stdin < /path/to/private-token.txt
  tufo server restart

Input must contain one token, with optional surrounding whitespace, then EOF.
The read is limited to 4096 bytes and 30 seconds. Ctrl-C cancels it.
The token is saved for the next server start; restarting stops current playback."#)]
    Token {
        /// Read the token from this file, never from a command-line value.
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with = "stdin",
            required_unless_present = "stdin"
        )]
        file: Option<PathBuf>,
        /// Read the token from standard input until EOF.
        #[arg(long, conflicts_with = "file")]
        stdin: bool,
    },
    /// Enable the configured bot's Discord connection.
    Enable,
    /// Log the bot out and move any Discord playback to local output.
    Disable,
}

enum TokenSource {
    File(PathBuf),
    Stdin,
}

struct BotToken(String);

impl fmt::Debug for BotToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BotToken(<redacted>)")
    }
}

impl BotToken {
    fn parse(bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.len() > TOKEN_BYTES_MAX {
            return Err("Discord token input exceeds 4096 bytes".to_owned());
        }
        let text =
            String::from_utf8(bytes).map_err(|_| "Discord token input must be UTF-8".to_owned())?;
        let token = text.trim();
        if token.is_empty() {
            return Err("Discord token input is empty".to_owned());
        }
        if !token.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(
                "Discord token must be one printable ASCII token without whitespace".to_owned(),
            );
        }
        Ok(Self(token.to_owned()))
    }
}

pub(crate) async fn run_discord(command: DiscordCommand, json: bool) -> anyhow::Result<bool> {
    match command {
        DiscordCommand::Status => {
            let client = crate::api::connect().await?;
            let status = client.server_status().await?;
            let state = crate::api::player_state(&client).await?;
            let bot_token = match status.discord_bot_token {
                DiscordBotTokenStatus::Missing => "missing",
                DiscordBotTokenStatus::Configured => "configured",
                DiscordBotTokenStatus::Invalid => "invalid",
            };
            let connection = match status.discord_connection {
                DiscordConnectionStatus::NotConfigured => "not_configured",
                DiscordConnectionStatus::Connecting => "connecting",
                DiscordConnectionStatus::Ready => "ready",
                DiscordConnectionStatus::Disabled => "disabled",
            };
            crate::api::emit(
                &json!({
                    "bot_token": bot_token,
                    "connection": connection,
                    "voice": crate::record::voice(&state.voice),
                    "output": crate::record::output(state.output),
                }),
                json,
            )
            .await?;
        }
        DiscordCommand::Channels => {
            let client = crate::api::connect().await?;
            let channels = client.list_voice_channels().await?;
            let records = channels
                .iter()
                .map(crate::record::voice_channel)
                .collect::<Vec<_>>();
            crate::api::emit(&json!(records), json).await?;
        }
        DiscordCommand::Enable => {
            let client = crate::api::connect().await?;
            client.enable_discord_connection().await?;
            crate::api::ack("enable", "discord", "connection", json).await?;
        }
        DiscordCommand::Disable => {
            let client = crate::api::connect().await?;
            client.disable_discord_connection().await?;
            crate::api::ack("disable", "discord", "connection", json).await?;
        }
        DiscordCommand::Token { file, stdin } => {
            let source = match (file, stdin) {
                (Some(path), false) => TokenSource::File(path),
                (None, true) => TokenSource::Stdin,
                _ => {
                    return Err(anyhow!(
                        "select exactly one token source: --file PATH or --stdin"
                    ));
                }
            };
            let Some(token) = read_token(source).await? else {
                return Ok(true);
            };
            let client = crate::api::connect().await?;
            client
                .set_discord_bot_token(token.0)
                .await
                .map_err(|_| anyhow!("could not save the Discord bot token"))?;
            crate::api::emit(
                &json!({
                    "action": "set",
                    "resource": "discord_token",
                    "id": "bot",
                    "restart_required": true,
                    "next_command": "tufo server restart",
                }),
                json,
            )
            .await?;
        }
    }
    Ok(true)
}

async fn read_token(source: TokenSource) -> anyhow::Result<Option<BotToken>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    // A detached reader lets timeout and Ctrl-C finish even while a pipe waits for EOF.
    std::thread::Builder::new()
        .name("tufo-discord-token".to_owned())
        .spawn(move || {
            let result = match source {
                TokenSource::File(path) => File::open(path)
                    .map_err(|_| "could not open the Discord token file".to_owned())
                    .and_then(read_token_bytes),
                TokenSource::Stdin => read_token_bytes(std::io::stdin().lock()),
            };
            let _ = sender.send(result);
        })
        .map_err(|_| anyhow!("could not start the Discord token reader"))?;
    tokio::select! {
        biased;
        result = tokio::signal::ctrl_c() => {
            result.context("listen for Ctrl-C")?;
            Ok(None)
        }
        result = tokio::time::timeout(TOKEN_INPUT_TIMEOUT, receiver) => {
            let result = result
                .map_err(|_| anyhow!("Discord token input did not finish within 30 seconds"))?
                .map_err(|_| anyhow!("Discord token reader stopped before completing"))?;
            result.map(Some).map_err(anyhow::Error::msg)
        }
    }
}

fn read_token_bytes(reader: impl std::io::Read) -> Result<BotToken, String> {
    let mut bytes = Vec::with_capacity(TOKEN_BYTES_MAX + 1);
    reader
        .take((TOKEN_BYTES_MAX + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "could not read the Discord token input".to_owned())?;
    BotToken::parse(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_input_is_trimmed_and_debug_is_redacted() -> Result<(), String> {
        let token = read_token_bytes("  abc.def.ghi\r\n".as_bytes())?;

        assert_eq!(token.0, "abc.def.ghi");
        assert_eq!(format!("{token:?}"), "BotToken(<redacted>)");
        Ok(())
    }

    #[test]
    fn token_input_is_bounded_and_errors_never_quote_it() {
        for bytes in [
            Vec::new(),
            b"   \r\n".to_vec(),
            b"SECRET TOKEN".to_vec(),
            b"SECRET\nTOKEN".to_vec(),
            b"SECRET\0TOKEN".to_vec(),
            b"SECRET\xffTOKEN".to_vec(),
            vec![b'x'; TOKEN_BYTES_MAX + 1],
        ] {
            let result = read_token_bytes(bytes.as_slice());
            assert!(result.is_err());
            if let Err(error) = result {
                assert!(!error.contains("SECRET"));
                assert!(!error.contains("TOKEN"));
            }
        }
    }

    #[test]
    fn token_reader_never_consumes_more_than_its_bound() {
        let mut reader = std::io::Cursor::new(vec![b'x'; TOKEN_BYTES_MAX * 2]);

        assert!(read_token_bytes(&mut reader).is_err());
        assert_eq!(reader.position(), (TOKEN_BYTES_MAX + 1) as u64);
    }
}
