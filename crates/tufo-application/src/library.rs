//! The library: audio files Tufo points at, kept in step with the folders that hold them.
//!
//! A file stays while a collection lists it or a source keeps it. Song sources are the followed
//! music folders and the files added to a playlist by their own path, so deleting a playlist
//! leaves its songs in the library.

pub mod add_files_to_collection;
pub mod add_songs_to_playlist;
pub mod create_collection;
pub mod delete_collection;
pub mod delete_collection_entry;
pub mod delete_library_folder;
pub mod delete_song;
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
use tufo_wire::library::{KenkuImportStatus, LibrarySnapshot};

use self::library_files::LibraryFiles;
use crate::error::ErrorMeta;

/// A failure of the library's database, which callers report without detail.
#[derive(Debug, thiserror::Error, ErrorMeta)]
#[error("the library database failed")]
#[meta(class = Internal, reason = Self::REASON)]
pub struct LibraryStorageError(#[source] pub anyhow::Error);

impl LibraryStorageError {
    pub const REASON: &'static str = "library.storage";
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
    kenku_imports: import_kenku_library::journal::KenkuImports,
}

impl<F: LibraryFiles> Library<F> {
    /// Opens the library over a migrated database and publishes its first snapshot.
    pub async fn open(database: Db, files: F) -> Result<Self, LibraryStorageError> {
        let mut reader = database.clone();
        let first = LibrarySnapshot {
            revision: 1,
            ..snapshot::load_snapshot(&mut reader).await?
        };
        let kenku_imports = import_kenku_library::journal::KenkuImports::open(&mut reader).await?;
        Ok(Self {
            database,
            files,
            write_gate: Mutex::new(()),
            snapshot: watch::Sender::new(Arc::new(first)),
            kenku_imports,
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

    #[must_use]
    pub fn watch_kenku_import(&self) -> watch::Receiver<Option<Arc<KenkuImportStatus>>> {
        self.kenku_imports.watch()
    }

    fn files(&self) -> &F {
        &self.files
    }

    async fn begin_change(&self) -> MutexGuard<'_, ()> {
        self.write_gate.lock().await
    }

    /// Reloads the library and publishes it as the next revision.
    async fn publish(&self) -> Result<(), LibraryStorageError> {
        let mut reader = self.database();
        let mut next = snapshot::load_snapshot(&mut reader).await?;
        self.snapshot.send_modify(|current| {
            next.revision = current.revision + 1;
            *current = Arc::new(next);
        });
        Ok(())
    }
}
