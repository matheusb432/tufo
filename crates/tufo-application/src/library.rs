//! The library: audio files Tufo points at, kept in step with the folders that hold them.

pub mod add_files_to_collection;
pub mod create_collection;
pub mod delete_collection;
pub mod delete_collection_entry;
pub mod folder_watcher;
pub mod import_kenku_library;
pub mod import_paths;
pub mod library_files;
mod manual_files;
pub mod reconcile;
pub mod seed_default_effects;
mod snapshot;
mod store;
pub mod sync_library;
pub mod update_collection;
pub mod update_collection_entry;

use std::sync::Arc;

use toasty::Db;
use tokio::sync::{Mutex, MutexGuard, watch};
use tufo_wire::library::LibrarySnapshot;

use self::library_files::LibraryFiles;

/// A failure of the library's database, which callers report without detail.
#[derive(Debug, thiserror::Error)]
#[error("the library database failed")]
pub struct LibraryStorageError(#[source] pub anyhow::Error);

impl LibraryStorageError {
    pub const REASON: &'static str = "library.storage";
}

impl crate::error::ApplicationError for LibraryStorageError {
    fn metadata(&self) -> crate::error::ErrorMetadata {
        crate::error::ErrorMetadata {
            class: crate::error::ErrorClass::Internal,
            reason: Self::REASON,
        }
    }
}

impl From<toasty::Error> for LibraryStorageError {
    fn from(error: toasty::Error) -> Self {
        Self(error.into())
    }
}

impl From<anyhow::Error> for LibraryStorageError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

/// The library's shared state: its database, the disk it reads, and the latest snapshot.
pub struct Library<F> {
    database: Db,
    files: F,
    /// Serializes every change, so reconciliation never plans against rows an edit is changing.
    write_gate: Mutex<()>,
    snapshot: watch::Sender<Arc<LibrarySnapshot>>,
}

impl<F: LibraryFiles> Library<F> {
    /// Opens the library over a migrated database and publishes its first snapshot.
    pub async fn open(database: Db, files: F) -> Result<Self, LibraryStorageError> {
        let mut reader = database.clone();
        let collections = snapshot::load_collections(&mut reader).await?;
        let first = LibrarySnapshot {
            revision: 1,
            collections,
        };
        Ok(Self {
            database,
            files,
            write_gate: Mutex::new(()),
            snapshot: watch::Sender::new(Arc::new(first)),
        })
    }

    /// Observes the current snapshot and every later one. A slow observer skips to the latest.
    #[must_use]
    pub fn watch(&self) -> watch::Receiver<Arc<LibrarySnapshot>> {
        self.snapshot.subscribe()
    }

    /// A handle on the library's database, which also keeps the player's settings.
    #[must_use]
    pub fn database(&self) -> Db {
        self.database.clone()
    }

    fn files(&self) -> &F {
        &self.files
    }

    async fn begin_change(&self) -> MutexGuard<'_, ()> {
        self.write_gate.lock().await
    }

    /// Reloads the collections and publishes them as the next revision.
    async fn publish(&self) -> Result<(), LibraryStorageError> {
        let mut reader = self.database();
        let collections = snapshot::load_collections(&mut reader).await?;
        self.snapshot.send_modify(|current| {
            *current = Arc::new(LibrarySnapshot {
                revision: current.revision + 1,
                collections,
            });
        });
        Ok(())
    }
}
