use super::bot_credentials::{BotCredentials, CredentialsError};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum SetDiscordBotTokenError {
    #[error("the Discord bot token is empty")]
    #[meta(class = InvalidArgument, reason = Self::EMPTY_TOKEN)]
    EmptyToken,
    #[error(transparent)]
    #[meta(transparent)]
    Credentials(#[from] CredentialsError),
}

impl SetDiscordBotTokenError {
    pub const EMPTY_TOKEN: &'static str = "discord.set_discord_bot_token.empty_token";
}

pub async fn execute(
    token: String,
    credentials: &impl BotCredentials,
) -> Result<(), SetDiscordBotTokenError> {
    let token = token.trim();
    if token.is_empty() {
        return Err(SetDiscordBotTokenError::EmptyToken);
    }
    credentials.save(token.to_owned()).await?;
    Ok(())
}
