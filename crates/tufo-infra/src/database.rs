//! The SQLite library database: where it lives, how connections behave, and its migrations.
//!
//! `tufo-server` is the database's only writer. It migrates the database once at startup, after
//! binding its control endpoint, so a second server never migrates concurrently.

mod driver;
mod migrations;
pub mod snapshot;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, bail};
use driver::{ConfiguredSqlite, DatabaseConnectionRole};
use toasty::Db;
use toasty_driver_sqlite::Sqlite;

const DATABASE_CONNECTIONS_MAX: usize = 4;
const DATABASE_MIGRATION_CONNECTIONS_MAX: usize = 1;
const DATABASE_WAIT_MAX: Duration = Duration::from_secs(5);
const DATABASE_PATH_ENVIRONMENT_VARIABLE: &str = "TUFO_DATABASE_PATH";
const DATABASE_FILE_NAME: &str = "tufo.sqlite3";

/// The database file: `TUFO_DATABASE_PATH` when set, otherwise `tufo.sqlite3` in the platform's
/// local data directory.
pub fn database_path() -> anyhow::Result<PathBuf> {
    if let Some(value) =
        std::env::var_os(DATABASE_PATH_ENVIRONMENT_VARIABLE).filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            bail!("{DATABASE_PATH_ENVIRONMENT_VARIABLE} must be an absolute path");
        }
        return Ok(path);
    }
    let directories = directories::ProjectDirs::from("", "", "tufo")
        .context("resolving the platform local data directory for the Tufo database")?;
    Ok(directories.data_local_dir().join(DATABASE_FILE_NAME))
}

/// Creates or migrates the database at `path`, then opens its runtime connection pool.
pub async fn open_database(path: &Path) -> anyhow::Result<Db> {
    let parent = path
        .parent()
        .context("the database path must have a parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating the database directory {}", parent.display()))?;

    let migration_driver = ConfiguredSqlite::new(path, DatabaseConnectionRole::Migration)
        .context("configuring the migration SQLite driver")?;
    let migration_database =
        connect_database(path, migration_driver, DATABASE_MIGRATION_CONNECTIONS_MAX).await?;
    migrations::migrate_database(&migration_database).await?;
    drop(migration_database);

    let runtime_driver = ConfiguredSqlite::new(path, DatabaseConnectionRole::Runtime)
        .context("configuring the runtime SQLite driver")?;
    let database = connect_database(path, runtime_driver, DATABASE_CONNECTIONS_MAX).await?;
    migrations::check_database_ready(&database).await?;
    Ok(database)
}

/// An empty in-memory database with every model registered, which migration authoring diffs
/// against the catalogue.
pub async fn build_authoring_database() -> anyhow::Result<Db> {
    database_builder(DATABASE_MIGRATION_CONNECTIONS_MAX)
        .build(Sqlite::in_memory())
        .await
        .context("building the in-memory migration authoring database")
}

async fn connect_database(
    path: &Path,
    driver: impl toasty::db::Driver,
    connections_max: usize,
) -> anyhow::Result<Db> {
    database_builder(connections_max)
        .build(driver)
        .await
        .with_context(|| format!("connecting to the database {}", path.display()))
}

fn database_builder(connections_max: usize) -> toasty::db::Builder {
    let mut builder = Db::builder();
    builder
        .models(toasty::models!(tufo_models::*))
        .max_pool_size(connections_max)
        .pool_wait_timeout(Some(DATABASE_WAIT_MAX))
        .pool_create_timeout(Some(DATABASE_WAIT_MAX));
    builder
}

/// A migrated database in a temporary directory, deleted when the returned directory drops.
#[cfg(any(test, feature = "test-support"))]
pub async fn open_temporary_database() -> anyhow::Result<(tempfile::TempDir, Db)> {
    let directory = tempfile::tempdir().context("creating a temporary database directory")?;
    let database = open_database(&directory.path().join(DATABASE_FILE_NAME)).await?;
    Ok((directory, database))
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use toasty::stmt::Uuid;
    use tufo_models::{
        library::{
            AudioFile, AudioFingerprint, Collection, CollectionEntry, CollectionId, CollectionKind,
            EntryOrderKey, FadeDuration, FileIdentity, LibraryFolder, SoundIcon, Title, Volume,
        },
        path::FolderPath,
        playback::AudioFilePath,
    };

    use super::*;

    async fn insert_audio_file(database: &mut Db, file_name: &str) -> AudioFile {
        toasty::create!(AudioFile {
            file_path: AudioFilePath::try_new(std::env::temp_dir().join(file_name)).unwrap(),
            file_identity: FileIdentity::try_new("unix:1:2".to_owned()).unwrap(),
            size_bytes: 1024,
            modified_ns: 0,
            fingerprint: AudioFingerprint::try_new("ab".repeat(32)).unwrap(),
            duration_ms: 90_000,
            title: Title::try_new("Tavern".to_owned()).unwrap(),
            missing_since: None,
            created_at: Timestamp::now(),
            song_source: false,
        })
        .exec(database)
        .await
        .unwrap()
    }

    async fn insert_manual_playlist(database: &mut Db) -> Collection {
        insert_manual_collection(database, CollectionKind::Playlist).await
    }

    async fn insert_manual_collection(database: &mut Db, kind: CollectionKind) -> Collection {
        insert_collection_created_at(database, kind, Timestamp::now()).await
    }

    async fn insert_collection_created_at(
        database: &mut Db,
        kind: CollectionKind,
        created_at: Timestamp,
    ) -> Collection {
        toasty::create!(Collection {
            kind,
            title: Title::try_new("Tavern night".to_owned()).unwrap(),
            background_image: None,
            order_key: EntryOrderKey::Title,
            order_descending: false,
            library_folder_id: None,
            folder_path: None,
            folder_identity: None,
            folder_missing_since: None,
            hidden_at: None,
            created_at,
            pinned: false,
        })
        .exec(database)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn reopening_a_migrated_database_keeps_its_rows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(DATABASE_FILE_NAME);
        let mut database = open_database(&path).await.unwrap();
        let audio_file = insert_audio_file(&mut database, "tavern.mp3").await;
        drop(database);

        let mut reopened = open_database(&path).await.unwrap();
        let stored = AudioFile::filter_by_id(audio_file.id)
            .first()
            .exec(&mut reopened)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(stored.file_path, audio_file.file_path);
        assert_eq!(stored.stamp(), audio_file.stamp());
    }

    #[tokio::test]
    async fn legacy_seed_titles_get_keys_without_changing_user_titles() {
        use tufo_models::library::{BuiltinTitle, default_effects::DefaultEffectsSeed};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(DATABASE_FILE_NAME);
        let mut database = open_database(&path).await.unwrap();
        let file = insert_audio_file(&mut database, "tavern.mp3").await;
        let collection = insert_manual_playlist(&mut database).await;
        let seeded_at = Timestamp::now();
        AudioFile::update_by_id(file.id)
            .title(Title::from_name_lossy("Rain"))
            .exec(&mut database)
            .await
            .unwrap();
        Collection::update_by_id(collection.id)
            .title(Title::from_name_lossy("My ambience"))
            .exec(&mut database)
            .await
            .unwrap();
        toasty::create!(DefaultEffectsSeed { id: 1, seeded_at })
            .exec(&mut database)
            .await
            .unwrap();
        toasty::create!(CollectionEntry {
            collection_id: collection.id,
            audio_file_id: file.id,
            added_at: seeded_at,
            position: 0,
            looping: true,
            volume: Volume::default(),
            fade_in: FadeDuration::default(),
            fade_out: FadeDuration::default(),
            icon: SoundIcon::CloudRain,
        })
        .exec(&mut database)
        .await
        .unwrap();
        drop(database);
        let legacy = rusqlite::Connection::open(&path).unwrap();
        legacy
            .execute_batch(
                "DROP TABLE song_loops;
             DROP TABLE kenku_imports;
             ALTER TABLE player_settings DROP COLUMN effects_volume;
             ALTER TABLE player_settings DROP COLUMN browser_volume;
             ALTER TABLE player_settings DROP COLUMN browser_muted;
             ALTER TABLE collections DROP COLUMN pinned;
             ALTER TABLE collections DROP COLUMN builtin_title;
             ALTER TABLE audio_files DROP COLUMN builtin_title;
             ALTER TABLE audio_files DROP COLUMN song_source;
             ALTER TABLE collection_entries DROP COLUMN position;
             DELETE FROM __toasty_migrations
             WHERE name IN ('0006_builtin_titles.sql', '0007_song_sources.sql', '0008_kenku_import_jobs.sql', '0009_effects_volume.sql', '0010_song_loops.sql', '0011_browser_settings.sql', '0012_pinned_collections.sql', '0013_custom_entry_order.sql');",
            )
            .unwrap();
        drop(legacy);

        let mut migrated = open_database(&path).await.unwrap();
        let file = AudioFile::filter_by_id(file.id)
            .first()
            .exec(&mut migrated)
            .await
            .unwrap()
            .unwrap();
        let collection = Collection::filter_by_id(collection.id)
            .first()
            .exec(&mut migrated)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(file.builtin_title, Some(BuiltinTitle::Rain));
        assert_eq!(file.title.as_str(), "Rain");
        assert_eq!(collection.builtin_title, None);
        assert_eq!(collection.title.as_str(), "My ambience");
    }

    async fn insert_entry(database: &mut Db, collection: &Collection, audio_file: &AudioFile) {
        toasty::create!(CollectionEntry {
            collection_id: collection.id,
            audio_file_id: audio_file.id,
            added_at: Timestamp::now(),
            position: 0,
            looping: false,
            volume: Volume::default(),
            fade_in: FadeDuration::default(),
            fade_out: FadeDuration::default(),
            icon: SoundIcon::default(),
        })
        .exec(database)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn files_in_hand_picked_playlists_become_song_sources() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(DATABASE_FILE_NAME);
        let mut database = open_database(&path).await.unwrap();
        let song = insert_audio_file(&mut database, "tavern.mp3").await;
        let sound = insert_audio_file(&mut database, "door.wav").await;
        let playlist = insert_manual_playlist(&mut database).await;
        let soundboard = insert_manual_collection(&mut database, CollectionKind::Soundboard).await;
        insert_entry(&mut database, &playlist, &song).await;
        insert_entry(&mut database, &soundboard, &sound).await;
        drop(database);
        let legacy = rusqlite::Connection::open(&path).unwrap();
        legacy
            .execute_batch(
                "DROP TABLE song_loops;
             DROP TABLE kenku_imports;
             ALTER TABLE player_settings DROP COLUMN effects_volume;
             ALTER TABLE player_settings DROP COLUMN browser_volume;
             ALTER TABLE player_settings DROP COLUMN browser_muted;
             ALTER TABLE audio_files DROP COLUMN song_source;
             ALTER TABLE collections DROP COLUMN pinned;
             ALTER TABLE collection_entries DROP COLUMN position;
             DELETE FROM __toasty_migrations WHERE name IN ('0007_song_sources.sql', '0008_kenku_import_jobs.sql', '0009_effects_volume.sql', '0010_song_loops.sql', '0011_browser_settings.sql', '0012_pinned_collections.sql', '0013_custom_entry_order.sql');",
            )
            .unwrap();
        drop(legacy);

        let mut migrated = open_database(&path).await.unwrap();
        let song_sources = AudioFile::all()
            .exec(&mut migrated)
            .await
            .unwrap()
            .into_iter()
            .map(|file| (file.id, file.song_source))
            .collect::<std::collections::HashMap<_, _>>();
        assert!(song_sources[&song.id]);
        assert!(!song_sources[&sound.id]);
    }

    #[tokio::test]
    async fn the_five_newest_shown_collections_of_each_kind_start_pinned() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(DATABASE_FILE_NAME);
        let mut database = open_database(&path).await.unwrap();
        let first_created_at = Timestamp::from_second(1_790_000_000).unwrap();
        let created_at = |offset_ms: i64| {
            first_created_at
                .checked_add(jiff::SignedDuration::from_millis(offset_ms))
                .unwrap()
        };
        // The cut falls between a whole second and the half second after it, which only a
        // fixed-width timestamp text orders correctly.
        let mut playlists = Vec::new();
        for offset_ms in [0, 1_000, 1_500, 2_000, 2_500, 3_000, 3_500] {
            playlists.push(
                insert_collection_created_at(
                    &mut database,
                    CollectionKind::Playlist,
                    created_at(offset_ms),
                )
                .await
                .id,
            );
        }
        let music = FolderPath::try_new(std::env::temp_dir().join("rpg")).unwrap();
        let identity = FileIdentity::try_new("unix:1:3".to_owned()).unwrap();
        let library_folder = toasty::create!(LibraryFolder {
            kind: CollectionKind::Playlist,
            folder_path: music.clone(),
            folder_identity: identity.clone(),
            created_at: created_at(0),
        })
        .exec(&mut database)
        .await
        .unwrap();
        let deleted_newest = insert_collection_created_at(
            &mut database,
            CollectionKind::Playlist,
            created_at(4_000),
        )
        .await
        .id;
        Collection::update_by_id(deleted_newest)
            .library_folder_id(Some(library_folder.id))
            .folder_path(Some(music))
            .folder_identity(Some(identity))
            .hidden_at(Some(created_at(5_000)))
            .exec(&mut database)
            .await
            .unwrap();
        let mut soundboards = Vec::new();
        for offset_ms in [100, 200] {
            soundboards.push(
                insert_collection_created_at(
                    &mut database,
                    CollectionKind::Soundboard,
                    created_at(offset_ms),
                )
                .await
                .id,
            );
        }
        drop(database);
        let legacy = rusqlite::Connection::open(&path).unwrap();
        legacy
            .execute_batch(
                "ALTER TABLE collections DROP COLUMN pinned;
             ALTER TABLE collection_entries DROP COLUMN position;
             DELETE FROM __toasty_migrations
             WHERE name IN ('0012_pinned_collections.sql', '0013_custom_entry_order.sql');",
            )
            .unwrap();
        drop(legacy);

        let mut migrated = open_database(&path).await.unwrap();
        let pinned = Collection::all()
            .exec(&mut migrated)
            .await
            .unwrap()
            .into_iter()
            .filter(|collection| collection.pinned)
            .map(|collection| collection.id)
            .collect::<std::collections::HashSet<_>>();
        let expected = playlists[2..]
            .iter()
            .chain(&soundboards)
            .copied()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(pinned, expected);
    }

    #[tokio::test]
    async fn deleting_a_collection_deletes_its_entries_but_not_their_files() {
        let (_directory, mut database) = open_temporary_database().await.unwrap();
        let audio_file = insert_audio_file(&mut database, "tavern.mp3").await;
        let collection = insert_manual_playlist(&mut database).await;
        toasty::create!(CollectionEntry {
            collection_id: collection.id,
            audio_file_id: audio_file.id,
            added_at: Timestamp::now(),
            position: 0,
            looping: false,
            volume: Volume::default(),
            fade_in: FadeDuration::default(),
            fade_out: FadeDuration::default(),
            icon: SoundIcon::default(),
        })
        .exec(&mut database)
        .await
        .unwrap();

        Collection::filter_by_id(collection.id)
            .delete()
            .exec(&mut database)
            .await
            .unwrap();

        let entries = CollectionEntry::all().exec(&mut database).await.unwrap();
        let audio_files = AudioFile::all().exec(&mut database).await.unwrap();
        assert!(entries.is_empty());
        assert_eq!(audio_files.len(), 1);
    }

    async fn insert_ordered_playlist(
        database: &mut Db,
        order_key: EntryOrderKey,
        order_descending: bool,
        files: &[AudioFile],
    ) -> Collection {
        let collection = insert_manual_playlist(database).await;
        Collection::update_by_id(collection.id)
            .order_key(order_key)
            .order_descending(order_descending)
            .exec(database)
            .await
            .unwrap();
        for file in files {
            insert_entry(database, &collection, file).await;
        }
        collection
    }

    async fn positions_by_file(
        database: &mut Db,
        collection: &Collection,
        files: &[AudioFile],
    ) -> Vec<u32> {
        let entries = CollectionEntry::filter_by_collection_id(collection.id)
            .exec(database)
            .await
            .unwrap();
        let position_of = |file: &AudioFile| {
            entries
                .iter()
                .find(|entry| entry.audio_file_id == file.id)
                .map(|entry| entry.position)
        };
        files
            .iter()
            .map(|file| position_of(file).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn the_custom_order_starts_as_each_collections_current_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(DATABASE_FILE_NAME);
        let mut database = open_database(&path).await.unwrap();
        let first_added_at = Timestamp::from_second(1_790_000_000).unwrap();
        let mut files = Vec::new();
        for (added_second, (file_name, duration_ms)) in (0..).zip([
            ("ambush.ogg", 180_000),
            ("tavern.ogg", 60_000),
            ("chase.ogg", 120_000),
        ]) {
            let file = insert_audio_file(&mut database, file_name).await;
            AudioFile::update_by_id(file.id)
                .duration_ms(duration_ms)
                .created_at(
                    first_added_at
                        .checked_add(jiff::SignedDuration::from_secs(added_second))
                        .unwrap(),
                )
                .exec(&mut database)
                .await
                .unwrap();
            files.push(file);
        }
        let longest_first =
            insert_ordered_playlist(&mut database, EntryOrderKey::Duration, true, &files).await;
        let oldest_first =
            insert_ordered_playlist(&mut database, EntryOrderKey::DateAdded, false, &files).await;
        let by_title =
            insert_ordered_playlist(&mut database, EntryOrderKey::Title, false, &files).await;
        drop(database);
        let legacy = rusqlite::Connection::open(&path).unwrap();
        legacy
            .execute_batch(
                "ALTER TABLE collection_entries DROP COLUMN position;
             DELETE FROM __toasty_migrations WHERE name = '0013_custom_entry_order.sql';",
            )
            .unwrap();
        drop(legacy);

        let mut migrated = open_database(&path).await.unwrap();
        assert_eq!(
            positions_by_file(&mut migrated, &longest_first, &files).await,
            [0, 2, 1]
        );
        assert_eq!(
            positions_by_file(&mut migrated, &oldest_first, &files).await,
            [0, 1, 2]
        );
        assert_eq!(
            positions_by_file(&mut migrated, &by_title, &files).await,
            [0, 0, 0]
        );
        drop(migrated);
        let migrated = rusqlite::Connection::open(&path).unwrap();
        let collection_indexes = migrated
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'index' AND tbl_name = 'collections' AND sql IS NOT NULL ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            collection_indexes,
            [
                "index_collections_by_library_folder_id",
                "index_collections_by_linked_folder"
            ]
        );
    }

    #[tokio::test]
    async fn a_linked_collection_needs_its_whole_folder() {
        let (_directory, mut database) = open_temporary_database().await.unwrap();
        let collection = insert_manual_playlist(&mut database).await;

        let half_linked =
            toasty::sql::statement("UPDATE collections SET folder_path = 'boss' WHERE id = ?1")
                .bind(collection.id.into_uuid().as_bytes().to_vec())
                .exec(&mut database)
                .await;

        assert!(half_linked.is_err());
    }

    #[tokio::test]
    async fn ids_round_trip_as_sixteen_byte_uuid_blobs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(DATABASE_FILE_NAME);
        let mut database = open_database(&path).await.unwrap();
        let created = insert_manual_playlist(&mut database).await;
        drop(database);
        let stored = rusqlite::Connection::open(&path).unwrap();
        let stored_ids = stored
            .prepare("SELECT id FROM collections")
            .unwrap()
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(stored_ids, [created.id.into_uuid().as_bytes().to_vec()]);
        let written_outside = Uuid::parse_str("0192f3a4-5b6c-7d8e-9f01-23456789abcd").unwrap();
        stored
            .execute(
                "UPDATE collections SET id = ?1",
                [written_outside.as_bytes().as_slice()],
            )
            .unwrap();
        drop(stored);

        let mut reopened = open_database(&path).await.unwrap();
        let loaded = Collection::filter_by_id(CollectionId::from_uuid(written_outside))
            .first()
            .exec(&mut reopened)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(loaded.id.into_uuid(), written_outside);
    }
}
