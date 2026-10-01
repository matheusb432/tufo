//! Saved outcomes and choices, plus one coalescing progress channel for the current import.

use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

use anyhow::Context as _;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use toasty::{Db, db::Executor};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};
use tufo_models::{
    kenku_import::{ImportProgress, KenkuImport, KenkuImportState},
    library::{CollectionId, CollectionKind, Title},
    playback::AudioFilePath,
};
use tufo_wire::library::{
    KenkuCollectionChoice, KenkuImportConflict, KenkuImportReport, KenkuImportStatus,
};

use crate::library::LibraryStorageError;

pub(in crate::library) struct KenkuImports {
    pub(super) admission: Mutex<()>,
    pub(super) worker: Mutex<Option<JoinHandle<()>>>,
    pub(super) stopping: AtomicBool,
    status: watch::Sender<Option<Arc<KenkuImportStatus>>>,
}

impl KenkuImports {
    pub(in crate::library) async fn open(database: &mut Db) -> Result<Self, LibraryStorageError> {
        let fields = KenkuImport::fields();
        KenkuImport::all()
            .filter(
                fields
                    .state()
                    .eq(KenkuImportState::Reading)
                    .or(fields.state().eq(KenkuImportState::Importing)),
            )
            .update()
            .state(KenkuImportState::Interrupted)
            .error_reason(Some(super::INTERRUPTED.to_owned()))
            .updated_at(Timestamp::now())
            .exec(database)
            .await?;
        let latest = KenkuImport::all()
            .order_by(KenkuImport::fields().updated_at().desc())
            .first()
            .exec(database)
            .await?;
        let status = latest.as_ref().map(status).transpose()?.map(Arc::new);
        Ok(Self {
            admission: Mutex::new(()),
            worker: Mutex::new(None),
            stopping: AtomicBool::new(false),
            status: watch::Sender::new(status),
        })
    }

    pub(in crate::library) fn watch(&self) -> watch::Receiver<Option<Arc<KenkuImportStatus>>> {
        self.status.subscribe()
    }

    pub(super) fn publish(&self, import: &KenkuImport) -> Result<(), LibraryStorageError> {
        self.status.send_replace(Some(Arc::new(status(import)?)));
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum StoredKind {
    Playlist,
    Soundboard,
}

impl From<CollectionKind> for StoredKind {
    fn from(kind: CollectionKind) -> Self {
        match kind {
            CollectionKind::Playlist => Self::Playlist,
            CollectionKind::Soundboard => Self::Soundboard,
        }
    }
}

impl From<StoredKind> for CollectionKind {
    fn from(kind: StoredKind) -> Self {
        match kind {
            StoredKind::Playlist => Self::Playlist,
            StoredKind::Soundboard => Self::Soundboard,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ImportedCollection {
    pub(super) source_id: String,
    pub(super) kind: StoredKind,
    /// None means the user chose to keep their existing collection.
    pub(super) destination: Option<String>,
}

impl ImportedCollection {
    pub(super) fn destination(&self) -> Result<Option<CollectionId>, LibraryStorageError> {
        self.destination.as_deref().map(collection_id).transpose()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StoredConflict {
    pub(super) source_id: String,
    pub(super) kind: StoredKind,
    pub(super) incoming_title: String,
    pub(super) existing_title: String,
    pub(super) target: String,
    /// The shown collection's metadata and entry IDs at the time choices were offered.
    pub(super) expected: String,
}

impl StoredConflict {
    pub(super) fn target(&self) -> Result<CollectionId, LibraryStorageError> {
        collection_id(&self.target)
    }

    fn view(&self) -> Result<KenkuImportConflict, LibraryStorageError> {
        Ok(KenkuImportConflict {
            source_id: self.source_id.clone(),
            kind: self.kind.into(),
            incoming_title: title(&self.incoming_title)?,
            existing_title: title(&self.existing_title)?,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct StoredReport {
    collections_imported: usize,
    files_imported: usize,
    skipped_collections: Vec<String>,
    unreadable_files: Vec<String>,
    unsupported_sources: Vec<String>,
    discord_bot_token_imported: bool,
}

impl From<&KenkuImportReport> for StoredReport {
    fn from(report: &KenkuImportReport) -> Self {
        Self {
            collections_imported: report.collections_imported,
            files_imported: report.files_imported,
            skipped_collections: report
                .skipped_collections
                .iter()
                .map(|title| title.as_str().to_owned())
                .collect(),
            unreadable_files: report
                .unreadable_files
                .iter()
                .map(|path| path.as_ref().to_string_lossy().into_owned())
                .collect(),
            unsupported_sources: report.unsupported_sources.clone(),
            discord_bot_token_imported: report.discord_bot_token_imported,
        }
    }
}

impl StoredReport {
    pub(super) fn report(&self) -> Result<KenkuImportReport, LibraryStorageError> {
        Ok(KenkuImportReport {
            collections_imported: self.collections_imported,
            files_imported: self.files_imported,
            skipped_collections: self
                .skipped_collections
                .iter()
                .map(|text| title(text))
                .collect::<Result<_, _>>()?,
            unreadable_files: self
                .unreadable_files
                .iter()
                .map(|text| {
                    AudioFilePath::try_new(PathBuf::from(text))
                        .context("invalid saved import path")
                        .map_err(Into::into)
                })
                .collect::<Result<_, LibraryStorageError>>()?,
            unsupported_sources: self.unsupported_sources.clone(),
            discord_bot_token_imported: self.discord_bot_token_imported,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Details {
    version: u32,
    pub(super) collections: Vec<ImportedCollection>,
    pub(super) conflicts: Vec<StoredConflict>,
    pub(super) report: StoredReport,
    pub(super) choices: Vec<StoredChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredChoice {
    source_id: String,
    kind: StoredKind,
    overwrite: bool,
}

impl From<&KenkuCollectionChoice> for StoredChoice {
    fn from(choice: &KenkuCollectionChoice) -> Self {
        Self {
            source_id: choice.source_id.clone(),
            kind: choice.kind.into(),
            overwrite: choice.overwrite,
        }
    }
}

impl From<&StoredChoice> for KenkuCollectionChoice {
    fn from(choice: &StoredChoice) -> Self {
        Self {
            source_id: choice.source_id.clone(),
            kind: choice.kind.into(),
            overwrite: choice.overwrite,
        }
    }
}

impl Default for Details {
    fn default() -> Self {
        Self {
            version: 1,
            collections: Vec::new(),
            conflicts: Vec::new(),
            report: StoredReport::default(),
            choices: Vec::new(),
        }
    }
}

impl Details {
    pub(super) fn read(import: &KenkuImport) -> Result<Self, LibraryStorageError> {
        let details: Self =
            serde_json::from_str(&import.details).context("decoding the saved Kenku import")?;
        if details.version != 1 {
            return Err(anyhow::anyhow!("unsupported Kenku import journal version").into());
        }
        for collection in &details.collections {
            collection.destination()?;
        }
        for conflict in &details.conflicts {
            conflict.target()?;
            conflict.view()?;
        }
        details.report.report()?;
        Ok(details)
    }

    pub(super) fn encode(&self) -> Result<String, LibraryStorageError> {
        Ok(serde_json::to_string(self).context("encoding the Kenku import journal")?)
    }
}

pub(super) async fn save(
    executor: &mut dyn Executor,
    import: &KenkuImport,
) -> Result<(), LibraryStorageError> {
    KenkuImport::update_by_id(import.id)
        .state(import.state)
        .progress(import.progress)
        .details(import.details.clone())
        .error_reason(import.error_reason.clone())
        .updated_at(Timestamp::now())
        .exec(executor)
        .await?;
    Ok(())
}

pub(super) async fn advance(
    database: &mut Db,
    imports: &KenkuImports,
    import: &mut KenkuImport,
    state: KenkuImportState,
    progress: u32,
) -> Result<(), LibraryStorageError> {
    import.state = state;
    import.progress = ImportProgress::try_new(progress).map_err(|error| anyhow::anyhow!(error))?;
    // A progress tick never rewrites the potentially large saved report or manifest.
    KenkuImport::update_by_id(import.id)
        .state(import.state)
        .progress(import.progress)
        .error_reason(import.error_reason.clone())
        .updated_at(Timestamp::now())
        .exec(database)
        .await?;
    imports.publish(import)
}

fn status(import: &KenkuImport) -> Result<KenkuImportStatus, LibraryStorageError> {
    let details = if import.state.is_running() {
        Details::default()
    } else {
        Details::read(import)?
    };
    Ok(KenkuImportStatus {
        id: import.id,
        state: import.state,
        progress: import.progress,
        conflicts: details
            .conflicts
            .iter()
            .map(StoredConflict::view)
            .collect::<Result<_, _>>()?,
        report: details.report.report()?,
        error_reason: import.error_reason.clone(),
    })
}

fn collection_id(text: &str) -> Result<CollectionId, LibraryStorageError> {
    Ok(CollectionId::from_uuid(
        uuid::Uuid::parse_str(text).context("invalid saved collection ID")?,
    ))
}

fn title(text: &str) -> Result<Title, LibraryStorageError> {
    Ok(Title::try_new(text.to_owned()).context("invalid saved import title")?)
}
