//! Durable state of a Kenku FM import. Audio remains in its original location.

use jiff::Timestamp;

use crate::{
    path::FolderPath,
    storage::{checked_column, uuid_identifier},
};

uuid_identifier!(
    /// One saved Kenku FM import and every attempt to complete it.
    KenkuImportId
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, toasty::Embed)]
pub enum KenkuImportState {
    Reading,
    AwaitingChoices,
    Importing,
    Succeeded,
    Failed,
    Interrupted,
}

impl KenkuImportState {
    #[must_use]
    pub fn is_running(self) -> bool {
        matches!(self, Self::Reading | Self::Importing)
    }
}

/// A coarse estimate, bounded to the range a progress bar accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportProgress(u32);

impl ImportProgress {
    pub const START: Self = Self(0);
    pub const COMPLETE: Self = Self(100);

    pub fn try_new(value: u32) -> Result<Self, &'static str> {
        if value <= 100 {
            Ok(Self(value))
        } else {
            Err("progress exceeds 100")
        }
    }

    #[must_use]
    pub fn percent(self) -> u32 {
        self.0
    }
}

checked_column!(
    ImportProgress,
    u32,
    decode = ImportProgress::try_new,
    encode = |progress| progress.percent(),
);

#[derive(Debug, toasty::Model)]
pub struct KenkuImport {
    #[key]
    #[auto]
    pub id: KenkuImportId,
    pub source_folder: Option<FolderPath>,
    pub state: KenkuImportState,
    pub progress: ImportProgress,
    /// Versioned, validated import outcomes and conflict choices. Never contains credentials.
    pub details: String,
    pub error_reason: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
