use std::future::Future;

use tufo_wire::server::DiscordBotTokenStatus;

use crate::error::{ApplicationError, ErrorClass, ErrorMetadata};

pub trait BotCredentials: Send + Sync {
    fn status(
        &self,
    ) -> impl Future<Output = Result<DiscordBotTokenStatus, CredentialsError>> + Send;
    fn save(&self, token: String) -> impl Future<Output = Result<(), CredentialsError>> + Send;
    fn save_unless_present(
        &self,
        token: String,
    ) -> impl Future<Output = Result<bool, CredentialsError>> + Send;
}

#[derive(Debug, thiserror::Error)]
#[error("the Discord credentials file cannot be accessed")]
pub struct CredentialsError(#[source] pub anyhow::Error);

impl CredentialsError {
    pub const REASON: &'static str = "discord.credentials.access";
}

impl ApplicationError for CredentialsError {
    fn metadata(&self) -> ErrorMetadata {
        ErrorMetadata {
            class: ErrorClass::Internal,
            reason: Self::REASON,
        }
    }
}
