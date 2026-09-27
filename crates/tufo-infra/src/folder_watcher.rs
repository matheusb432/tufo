//! Watching the library folders through the platform's file notification API: inotify on
//! Linux, `FSEvents` on macOS, and `ReadDirectoryChangesW` on Windows.

use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Mutex, PoisonError},
};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::sync::watch;
use tufo_application::library::folder_watcher::FolderWatcher;
use tufo_models::path::FolderPath;

/// Counts changes under the watched folders. Access events don't count; watcher errors do, so a
/// lost event still leads to a reconciliation.
pub struct NotifyFolderWatcher {
    watcher: Mutex<Watched>,
    changes: watch::Sender<u64>,
}

struct Watched {
    /// Absent when the platform refused to create a watcher; the periodic reconciliation then
    /// does all the work.
    watcher: Option<RecommendedWatcher>,
    folders: BTreeSet<PathBuf>,
}

impl NotifyFolderWatcher {
    #[must_use]
    pub fn start() -> Self {
        let changes = watch::Sender::new(0_u64);
        let sender = changes.clone();
        let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            let counts = match &result {
                Ok(event) => !matches!(event.kind, EventKind::Access(_)),
                Err(error) => {
                    tracing::warn!(%error, "the folder watcher reported an error");
                    true
                }
            };
            if counts {
                sender.send_modify(|count| *count = count.wrapping_add(1));
            }
        })
        .inspect_err(|error| tracing::warn!(%error, "cannot create a folder watcher"))
        .ok();
        Self {
            watcher: Mutex::new(Watched {
                watcher,
                folders: BTreeSet::new(),
            }),
            changes,
        }
    }
}

impl FolderWatcher for NotifyFolderWatcher {
    fn watch_folders(&self, folders: Vec<FolderPath>) {
        let wanted = folders
            .into_iter()
            .map(|folder| folder.as_ref().clone())
            .collect::<BTreeSet<_>>();
        let mut state = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
        let Watched { watcher, folders } = &mut *state;
        let Some(watcher) = watcher else {
            return;
        };
        for removed in folders.difference(&wanted) {
            if let Err(error) = watcher.unwatch(removed) {
                tracing::debug!(folder = %removed.display(), %error, "cannot unwatch a folder");
            }
        }
        let mut now_watched = BTreeSet::new();
        for folder in wanted {
            let watching = if folders.contains(&folder) {
                Ok(())
            } else {
                watcher.watch(&folder, RecursiveMode::Recursive)
            };
            match watching {
                Ok(()) => {
                    now_watched.insert(folder);
                }
                Err(error) => {
                    tracing::warn!(folder = %folder.display(), %error, "cannot watch a library folder");
                }
            }
        }
        *folders = now_watched;
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn a_new_file_in_a_watched_folder_counts_as_a_change() {
        let folder = tempfile::tempdir().unwrap();
        let nested = folder.path().join("boss");
        std::fs::create_dir(&nested).unwrap();
        let watcher = NotifyFolderWatcher::start();
        let mut changes = watcher.changes();
        watcher.watch_folders(vec![
            FolderPath::try_new(folder.path().to_path_buf()).unwrap(),
        ]);

        std::fs::write(nested.join("dragon.wav"), b"RIFF").unwrap();

        tokio::time::timeout(Duration::from_secs(5), changes.changed())
            .await
            .unwrap()
            .unwrap();
    }
}
