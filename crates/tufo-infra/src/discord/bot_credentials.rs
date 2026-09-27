use std::path::PathBuf;

use tufo_application::discord::bot_credentials::{BotCredentials, CredentialsError};
use tufo_wire::server::DiscordBotTokenStatus;

use super::{DiscordCredentials, LoadCredentialsError};

#[derive(Debug, Clone)]
pub struct FileBotCredentials {
    path: PathBuf,
}

impl FileBotCredentials {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl BotCredentials for FileBotCredentials {
    async fn status(&self) -> Result<DiscordBotTokenStatus, CredentialsError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || match DiscordCredentials::load(&path) {
            Ok(_) => DiscordBotTokenStatus::Configured,
            Err(LoadCredentialsError::Read(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                DiscordBotTokenStatus::Missing
            }
            Err(_) => DiscordBotTokenStatus::Invalid,
        })
        .await
        .map_err(|error| CredentialsError(error.into()))
    }

    async fn save(&self, token: String) -> Result<(), CredentialsError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || DiscordCredentials::save(&path, &token))
            .await
            .map_err(|error| CredentialsError(error.into()))?
            .map_err(|error| CredentialsError(error.into()))
    }

    async fn save_unless_present(&self, token: String) -> Result<bool, CredentialsError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || DiscordCredentials::save_unless_present(&path, &token))
            .await
            .map_err(|error| CredentialsError(error.into()))?
            .map_err(|error| CredentialsError(error.into()))
    }
}
