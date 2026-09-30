use std::future::Future;

use tufo_wire::server::DiscordBotTokenStatus;

use crate::error::ErrorMeta;

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

#[derive(Debug, thiserror::Error, ErrorMeta)]
#[error("the Discord credentials file cannot be accessed")]
#[meta(class = Internal, reason = Self::REASON)]
pub struct CredentialsError(#[source] pub anyhow::Error);

impl CredentialsError {
    pub const REASON: &'static str = "discord.credentials.access";
}
