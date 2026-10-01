use std::{fmt, future::Future};

use tufo_models::path::FolderPath;

use super::KenkuLibrary;
use crate::error::ErrorMeta;

pub trait KenkuReader: Send + Sync {
    fn read(
        &self,
        folder: Option<FolderPath>,
    ) -> impl Future<Output = Result<KenkuData, ReadKenkuDataError>> + Send;
}

pub struct KenkuData {
    pub library: KenkuLibrary,
    pub discord_bot_token: Option<String>,
}

impl fmt::Debug for KenkuData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KenkuData")
            .field("library", &self.library)
            .field(
                "discord_bot_token",
                &self.discord_bot_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum ReadKenkuDataError {
    #[error("close Kenku FM and try again")]
    #[meta(class = FailedPrecondition, reason = Self::RUNNING)]
    Running,
    #[error("the folder holds no Kenku FM library")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error("cannot read Kenku FM's storage; quit Kenku FM and try again")]
    #[meta(class = FailedPrecondition, reason = Self::UNREADABLE)]
    Unreadable(#[source] anyhow::Error),
    #[error("Kenku FM's library is malformed")]
    #[meta(class = FailedPrecondition, reason = Self::MALFORMED)]
    Malformed(#[source] anyhow::Error),
    #[error("reading Kenku FM's library stopped")]
    #[meta(class = Internal, reason = Self::INTERNAL)]
    Internal(#[source] anyhow::Error),
}

impl ReadKenkuDataError {
    pub const RUNNING: &'static str = "library.kenku_data.running";
    pub const NOT_FOUND: &'static str = "library.kenku_data.not_found";
    pub const UNREADABLE: &'static str = "library.kenku_data.unreadable";
    pub const MALFORMED: &'static str = "library.kenku_data.malformed";
    pub const INTERNAL: &'static str = "library.kenku_data.internal";
}
