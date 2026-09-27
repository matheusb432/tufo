//! Following the library folders while the server runs.
//!
//! The server reconciles once at start, then again after each burst of filesystem events
//! settles. A periodic reconciliation catches whatever the watcher missed: network mounts that
//! send no events, or a watch limit the operating system refused.

use std::{future::Future, time::Duration};

use tokio::sync::watch;
use tufo_models::{library::LibraryFolder, path::FolderPath};

use super::{Library, library_files::LibraryFiles, sync_library};

/// How long the disk must stay quiet before a burst of events triggers a reconciliation, so a
/// copy in progress is read once it finishes.
const EVENTS_SETTLE_DELAY: Duration = Duration::from_secs(2);
/// The longest a steady stream of events can postpone a reconciliation.
const EVENTS_DELAY_MAX: Duration = Duration::from_secs(10);
const PERIODIC_SYNC_INTERVAL: Duration = Duration::from_mins(10);

/// Watches folders recursively for changes.
pub trait FolderWatcher: Send + Sync {
    /// Watches exactly `folders`, replacing the previous set. A folder that can't be watched is
    /// logged and left to the periodic reconciliation.
    fn watch_folders(&self, folders: Vec<FolderPath>);

    /// A counter that increases on every change under a watched folder.
    fn changes(&self) -> watch::Receiver<u64>;
}

/// Keeps the library in step with the disk until `shutdown` resolves.
pub async fn run<F: LibraryFiles>(
    library: &Library<F>,
    watcher: &impl FolderWatcher,
    shutdown: impl Future<Output = ()>,
) {
    tokio::pin!(shutdown);
    let mut changes = watcher.changes();
    loop {
        changes.mark_unchanged();
        sync_and_rewatch(library, watcher).await;
        tokio::select! {
            () = &mut shutdown => return,
            () = tokio::time::sleep(PERIODIC_SYNC_INTERVAL) => {}
            watching = changes.changed() => {
                if watching.is_err() {
                    return;
                }
                tokio::select! {
                    () = &mut shutdown => return,
                    () = settle(&mut changes) => {}
                }
            }
        }
    }
}

/// Waits until no change arrives for [`EVENTS_SETTLE_DELAY`], or [`EVENTS_DELAY_MAX`] passes.
async fn settle(changes: &mut watch::Receiver<u64>) {
    let deadline = tokio::time::Instant::now() + EVENTS_DELAY_MAX;
    loop {
        let quiet = tokio::time::sleep(EVENTS_SETTLE_DELAY);
        tokio::select! {
            () = quiet => return,
            () = tokio::time::sleep_until(deadline) => return,
            watching = changes.changed() => {
                if watching.is_err() {
                    return;
                }
            }
        }
    }
}

async fn sync_and_rewatch<F: LibraryFiles>(library: &Library<F>, watcher: &impl FolderWatcher) {
    match sync_library::execute(library).await {
        Ok(report) if report.changed => tracing::info!(
            collections_created = report.collections_created,
            files_added = report.files_added,
            unreadable_files = report.unreadable_files.len(),
            "reconciled the library with the disk"
        ),
        Ok(_) => {}
        Err(error) => tracing::error!(%error, "cannot reconcile the library"),
    }
    let mut database = library.database();
    match LibraryFolder::all().exec(&mut database).await {
        Ok(folders) => watcher.watch_folders(
            folders
                .into_iter()
                .map(|folder| folder.folder_path)
                .collect(),
        ),
        Err(error) => tracing::error!(%error, "cannot list the library folders to watch"),
    }
}
