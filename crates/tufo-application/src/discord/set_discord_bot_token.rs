use super::bot_credentials::{BotCredentials, CredentialsError};
use crate::error::{ApplicationError, ErrorClass, ErrorMetadata};

#[derive(Debug, thiserror::Error)]
pub enum SetDiscordBotTokenError {
    #[error("the Discord bot token is empty")]
    EmptyToken,
    #[error(transparent)]
    Credentials(#[from] CredentialsError),
}

impl SetDiscordBotTokenError {
    pub const EMPTY_TOKEN: &'static str = "discord.set_discord_bot_token.empty_token";
}

impl ApplicationError for SetDiscordBotTokenError {
    fn metadata(&self) -> ErrorMetadata {
        match self {
            Self::EmptyToken => ErrorMetadata {
                class: ErrorClass::InvalidArgument,
                reason: Self::EMPTY_TOKEN,
            },
            Self::Credentials(error) => error.metadata(),
        }
    }
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
