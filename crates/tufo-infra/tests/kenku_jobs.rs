//! Job admission, deadlines and restart recovery over a real migrated database.

use std::{sync::Arc, time::Duration};

use anyhow::Context as _;
use tokio::sync::{Notify, watch};
use tufo_application::library::{
    Library,
    import_kenku_library::{
        self, KenkuLibrary,
        kenku_data::{KenkuData, KenkuReader, ReadKenkuDataError},
    },
};
use tufo_infra::{
    database::open_database,
    discord::{SongbirdDiscordClient, bot_credentials::FileBotCredentials},
    library_files::DiskLibraryFiles,
    player::EnginePlayer,
};
use tufo_models::{
    kenku_import::{KenkuImport, KenkuImportState},
    path::FolderPath,
};
use tufo_wire::library::{ImportKenkuLibrary, KenkuImportStatus};

struct PausedReader {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl KenkuReader for PausedReader {
    async fn read(&self, _folder: Option<FolderPath>) -> Result<KenkuData, ReadKenkuDataError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(KenkuData {
            library: KenkuLibrary::default(),
            discord_bot_token: None,
        })
    }
}

async fn terminal(
    updates: &mut watch::Receiver<Option<Arc<KenkuImportStatus>>>,
) -> anyhow::Result<Arc<KenkuImportStatus>> {
    let status = tokio::time::timeout(
        Duration::from_secs(5),
        updates.wait_for(|update| {
            update
                .as_ref()
                .is_some_and(|status| !status.state.is_running())
        }),
    )
    .await??;
    Ok(status
        .as_ref()
        .context("the import stream has no terminal status")?
        .clone())
}

#[tokio::test]
async fn kenku_job_outlives_thirty_seconds_without_a_watcher_and_duplicate_starts_reuse_it() {
    let folder = tempfile::tempdir().unwrap();
    let database = open_database(&folder.path().join("library.sqlite3"))
        .await
        .unwrap();
    let library = Arc::new(
        Library::open(database.clone(), DiskLibraryFiles)
            .await
            .unwrap(),
    );
    let player = EnginePlayer::start(SongbirdDiscordClient::not_configured());
    let credentials = FileBotCredentials::new(folder.path().join("discord.toml"));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let id = import_kenku_library::execute(
        ImportKenkuLibrary::default(),
        library.clone(),
        player.clone(),
        PausedReader {
            entered: entered.clone(),
            release: release.clone(),
        },
        credentials.clone(),
    )
    .await
    .unwrap();
    entered.notified().await;
    let other_source = import_kenku_library::execute(
        ImportKenkuLibrary {
            kenku_folder: Some(FolderPath::try_new(folder.path().join("another-source")).unwrap()),
            ..ImportKenkuLibrary::default()
        },
        library.clone(),
        player.clone(),
        PausedReader {
            entered: entered.clone(),
            release: release.clone(),
        },
        credentials.clone(),
    )
    .await;
    assert!(matches!(
        other_source,
        Err(import_kenku_library::ImportKenkuLibraryError::Busy)
    ));
    let again = import_kenku_library::execute(
        ImportKenkuLibrary::default(),
        library.clone(),
        player,
        PausedReader {
            entered,
            release: release.clone(),
        },
        credentials,
    )
    .await
    .unwrap();
    assert_eq!(id, again);
    assert_eq!(
        KenkuImport::all()
            .exec(&mut database.clone())
            .await
            .unwrap()
            .len(),
        1
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();
    let mut updates = library.watch_kenku_import();
    assert!(updates.borrow().as_ref().unwrap().state.is_running());
    release.notify_one();
    let result = terminal(&mut updates).await.unwrap();
    assert_eq!(result.state, KenkuImportState::Succeeded);
    assert_eq!(result.progress.percent(), 100);
}

#[tokio::test]
async fn kenku_job_fails_at_five_minutes_and_its_failure_is_saved() {
    let folder = tempfile::tempdir().unwrap();
    let database = open_database(&folder.path().join("library.sqlite3"))
        .await
        .unwrap();
    let library = Arc::new(
        Library::open(database.clone(), DiskLibraryFiles)
            .await
            .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let id = import_kenku_library::execute(
        ImportKenkuLibrary::default(),
        library.clone(),
        EnginePlayer::start(SongbirdDiscordClient::not_configured()),
        PausedReader {
            entered: entered.clone(),
            release: Arc::new(Notify::new()),
        },
        FileBotCredentials::new(folder.path().join("discord.toml")),
    )
    .await
    .unwrap();
    entered.notified().await;
    tokio::time::pause();
    tokio::time::advance(import_kenku_library::IMPORT_TIMEOUT).await;
    tokio::time::resume();
    let result = terminal(&mut library.watch_kenku_import()).await.unwrap();
    assert_eq!(result.state, KenkuImportState::Failed);
    assert_eq!(
        result.error_reason.as_deref(),
        Some(import_kenku_library::TIMED_OUT)
    );
    let reopened = Library::open(database, DiskLibraryFiles).await.unwrap();
    let saved = reopened
        .watch_kenku_import()
        .borrow()
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(saved.id, id);
    assert_eq!(*saved, *result);
}

#[tokio::test]
async fn an_interrupted_kenku_job_can_resume_after_reopening_the_library() {
    let folder = tempfile::tempdir().unwrap();
    let database = open_database(&folder.path().join("library.sqlite3"))
        .await
        .unwrap();
    let library = Arc::new(
        Library::open(database.clone(), DiskLibraryFiles)
            .await
            .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let player = EnginePlayer::start(SongbirdDiscordClient::not_configured());
    let credentials = FileBotCredentials::new(folder.path().join("discord.toml"));
    let id = import_kenku_library::execute(
        ImportKenkuLibrary::default(),
        library.clone(),
        player.clone(),
        PausedReader {
            entered: entered.clone(),
            release: Arc::new(Notify::new()),
        },
        credentials.clone(),
    )
    .await
    .unwrap();
    entered.notified().await;
    import_kenku_library::interrupt(&library).await.unwrap();
    assert_eq!(
        terminal(&mut library.watch_kenku_import())
            .await
            .unwrap()
            .state,
        KenkuImportState::Interrupted
    );
    let reopened = Arc::new(Library::open(database, DiskLibraryFiles).await.unwrap());
    let release = Arc::new(Notify::new());
    release.notify_one();
    import_kenku_library::execute(
        ImportKenkuLibrary {
            import_id: Some(id),
            ..ImportKenkuLibrary::default()
        },
        reopened.clone(),
        player,
        PausedReader { entered, release },
        credentials,
    )
    .await
    .unwrap();
    let result = terminal(&mut reopened.watch_kenku_import()).await.unwrap();
    assert_eq!(result.id, id);
    assert_eq!(result.state, KenkuImportState::Succeeded);
}
