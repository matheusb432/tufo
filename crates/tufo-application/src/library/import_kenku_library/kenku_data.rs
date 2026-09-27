use std::{fmt, future::Future};

use tufo_models::path::FolderPath;

use super::KenkuLibrary;
use crate::error::{ApplicationError, ErrorClass, ErrorMetadata};

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

#[derive(Debug, thiserror::Error)]
pub enum ReadKenkuDataError {
    #[error("the folder holds no Kenku FM library")]
    NotFound,
    #[error("cannot read Kenku FM's storage; quit Kenku FM and try again")]
    Unreadable(#[source] anyhow::Error),
    #[error("Kenku FM's library is malformed")]
    Malformed(#[source] anyhow::Error),
    #[error("reading Kenku FM's library stopped")]
    Internal(#[source] anyhow::Error),
}

impl ReadKenkuDataError {
    pub const NOT_FOUND: &'static str = "library.kenku_data.not_found";
    pub const UNREADABLE: &'static str = "library.kenku_data.unreadable";
    pub const MALFORMED: &'static str = "library.kenku_data.malformed";
    pub const INTERNAL: &'static str = "library.kenku_data.internal";
}

impl ApplicationError for ReadKenkuDataError {
    fn metadata(&self) -> ErrorMetadata {
        match self {
            Self::NotFound => ErrorMetadata {
                class: ErrorClass::NotFound,
                reason: Self::NOT_FOUND,
            },
            Self::Unreadable(_) => ErrorMetadata {
                class: ErrorClass::FailedPrecondition,
                reason: Self::UNREADABLE,
            },
            Self::Malformed(_) => ErrorMetadata {
                class: ErrorClass::FailedPrecondition,
                reason: Self::MALFORMED,
            },
            Self::Internal(_) => ErrorMetadata {
                class: ErrorClass::Internal,
                reason: Self::INTERNAL,
            },
        }
    }
}
