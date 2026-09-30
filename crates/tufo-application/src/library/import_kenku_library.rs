//! Importing Kenku FM's playlists, soundboards, and player settings.

pub mod kenku_data;
mod kenku_library;
mod kenku_titles;
mod plan;

use std::collections::{HashMap, HashSet};

use toasty::Db;
use toasty_core::driver::operation::TransactionMode;
use tufo_models::{
    library::{
        AudioFile, Collection, CollectionEntry, CollectionId, CollectionKind, SoundSettings,
    },
    path::FolderPath,
    playback::{AudioFilePath, queue::RepeatMode},
};
use tufo_wire::{
    library::{ImportKenkuLibrary, KenkuImportReport},
    player::UpdatePlayerSettings,
};

pub use self::kenku_library::{
    KenkuBackground, KenkuCollection, KenkuItem, KenkuLibrary, KenkuPlayerSettings, KenkuSource,
};
use self::{
    kenku_data::{KenkuReader, ReadKenkuDataError},
    plan::{PlanInput, PlannedCollection, PlannedFile},
};
use super::{
    Library, LibraryStorageError, import_paths, library_files::LibraryFiles, manual_files, store,
    sync_library,
};
use crate::{
    discord::bot_credentials::BotCredentials,
    error::ErrorMeta,
    playback::{
        player::Player,
        update_player_settings::{self, UpdatePlayerSettingsError},
    },
};

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum ImportKenkuLibraryError {
    #[error(transparent)]
    #[meta(transparent)]
    Read(#[from] ReadKenkuDataError),
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
    #[error(transparent)]
    #[meta(transparent)]
    PlayerSettings(#[from] UpdatePlayerSettingsError),
}

pub async fn execute<F: LibraryFiles>(
    request: ImportKenkuLibrary,
    library: &Library<F>,
    player: &impl Player,
    reader: &impl KenkuReader,
    credentials: &impl BotCredentials,
) -> Result<KenkuImportReport, ImportKenkuLibraryError> {
    let data = reader.read(request.kenku_folder).await?;
    let kenku = data.library;
    let mut report = import_collections(kenku.collections, library).await?;
    let songs_queued = !player.watch_player().borrow().queued_entry_ids.is_empty();
    let update = settings_update(kenku.player_settings, songs_queued);
    if update != UpdatePlayerSettings::default() {
        update_player_settings::execute(update, player, library.database()).await?;
    }
    if let Some(token) = data.discord_bot_token {
        report.discord_bot_token_imported = match credentials.save_unless_present(token).await {
            Ok(saved) => saved,
            Err(error) => {
                tracing::warn!(%error, "cannot save Kenku FM's Discord bot token");
                false
            }
        };
    }
    Ok(report)
}

/// Songs queued now keep Kenku FM's repeat-one out, since repeating one track refuses them.
fn settings_update(kenku: KenkuPlayerSettings, songs_queued: bool) -> UpdatePlayerSettings {
    let KenkuPlayerSettings {
        volume,
        muted,
        shuffle,
        repeat,
    } = kenku;
    UpdatePlayerSettings {
        volume,
        muted,
        shuffle,
        repeat: repeat.filter(|repeat| !(songs_queued && *repeat == RepeatMode::Track)),
        crossfade: None,
    }
}

async fn import_collections<F: LibraryFiles>(
    collections: Vec<KenkuCollection>,
    library: &Library<F>,
) -> Result<KenkuImportReport, LibraryStorageError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let shown = Collection::all()
        .exec(&mut database)
        .await?
        .into_iter()
        .filter(|collection| collection.hidden_at.is_none())
        .collect::<Vec<_>>();
    let folder_audio_files = scan_folder_candidates(library.files(), &collections).await;
    let plan = plan::plan(PlanInput {
        existing: shown
            .iter()
            .map(|collection| (collection.kind, collection.title.clone()))
            .collect(),
        shown_presets: shown
            .iter()
            .filter_map(|collection| collection.background_preset)
            .collect(),
        collections,
        folder_audio_files,
        rng: fastrand::Rng::new(),
    });
    let mut report = KenkuImportReport {
        skipped_collections: plan.skipped,
        unsupported_sources: plan.unsupported_sources,
        ..KenkuImportReport::default()
    };

    let mut followed_any = false;
    for planned in &plan.collections {
        if let Some(folder) = &planned.folder {
            import_paths::follow_folder(
                &mut database,
                library.files(),
                planned.kind,
                folder.clone(),
            )
            .await?;
            followed_any = true;
        }
    }
    if followed_any {
        sync_library::sync_while_changing(library).await?;
    }
    let mut linked = shown_linked_collections(&mut database).await?;
    for planned in plan.collections {
        let linked_id = planned
            .folder
            .as_ref()
            .and_then(|folder| linked.remove(&(planned.kind, folder.clone())));
        if let Some(id) = linked_id {
            adopt_linked_collection(&mut database, id, &planned).await?;
            report.files_imported += planned.files.len();
            apply_file_settings(&mut database, id, &planned.files).await?;
        } else {
            let created = store::create_manual_collection(
                &mut database,
                planned.kind,
                planned.title.clone(),
                planned.background.clone(),
                None,
            )
            .await?;
            let added =
                manual_files::add_files(library, created.id, unique_paths(&planned.files)).await?;
            report.files_imported += added.files_added;
            report.unreadable_files.extend(added.unreadable);
            apply_file_settings(&mut database, created.id, &planned.files).await?;
        }
        report.collections_imported += 1;
    }
    library.publish().await?;
    Ok(report)
}

/// Skips folders whose scan was truncated.
async fn scan_folder_candidates(
    files: &impl LibraryFiles,
    collections: &[KenkuCollection],
) -> HashMap<FolderPath, Vec<AudioFilePath>> {
    let candidates = collections
        .iter()
        .filter_map(plan::folder_candidate)
        .collect::<HashSet<_>>();
    let mut scanned = HashMap::with_capacity(candidates.len());
    for folder in candidates {
        let Ok(scan) = files.scan_folder(folder.clone()).await else {
            continue;
        };
        if scan.truncated {
            continue;
        }
        if let Some(root) = scan
            .folders
            .into_iter()
            .find(|scanned_folder| scanned_folder.path == folder)
        {
            let audio_files = root.audio_files.into_iter().map(|file| file.path).collect();
            scanned.insert(folder, audio_files);
        }
    }
    scanned
}

async fn shown_linked_collections(
    database: &mut Db,
) -> Result<HashMap<(CollectionKind, FolderPath), CollectionId>, LibraryStorageError> {
    // Toasty 0.10 can't serialize a filter on an optional UUID column, so this filters in memory.
    Ok(Collection::all()
        .exec(database)
        .await?
        .into_iter()
        .filter(|collection| collection.hidden_at.is_none())
        .filter_map(|collection| {
            collection.library_folder_id?;
            Some(((collection.kind, collection.folder_path?), collection.id))
        })
        .collect())
}

async fn adopt_linked_collection(
    database: &mut Db,
    id: CollectionId,
    planned: &PlannedCollection,
) -> Result<(), LibraryStorageError> {
    let (background_preset, background_image) = planned.background.clone().into_columns();
    Collection::update_by_id(id)
        .title(planned.title.clone())
        .background_preset(background_preset)
        .background_image(background_image)
        .exec(database)
        .await?;
    Ok(())
}

/// Kenku FM lets a playlist list a file twice.
fn unique_paths(files: &[PlannedFile]) -> Vec<AudioFilePath> {
    let mut seen = HashSet::with_capacity(files.len());
    files
        .iter()
        .filter(|file| seen.insert(file.path.as_ref().clone()))
        .map(|file| file.path.clone())
        .collect()
}

async fn apply_file_settings(
    database: &mut Db,
    collection_id: CollectionId,
    files: &[PlannedFile],
) -> Result<(), LibraryStorageError> {
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await?;
    let entries = CollectionEntry::filter_by_collection_id(collection_id)
        .exec(&mut transaction)
        .await?;
    for file in files {
        let stored = AudioFile::filter_by_file_path(file.path.clone())
            .first()
            .exec(&mut transaction)
            .await?;
        let Some(stored) = stored else {
            continue;
        };
        if let Some(title) = &file.typed_title {
            AudioFile::update_by_id(stored.id)
                .title(title.clone())
                .exec(&mut transaction)
                .await?;
        }
        if file.sound == SoundSettings::default() {
            continue;
        }
        for entry in entries
            .iter()
            .filter(|entry| entry.audio_file_id == stored.id)
        {
            CollectionEntry::update_by_id(entry.id)
                .looping(file.sound.looping)
                .volume(file.sound.volume)
                .fade_in(file.sound.fade_in)
                .fade_out(file.sound.fade_out)
                .exec(&mut transaction)
                .await?;
        }
    }
    transaction.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_songs_keep_kenkus_repeat_one_out_of_the_settings() {
        let repeating_track = KenkuPlayerSettings {
            volume: None,
            muted: Some(true),
            shuffle: None,
            repeat: Some(RepeatMode::Track),
        };
        let repeating_playlist = KenkuPlayerSettings {
            repeat: Some(RepeatMode::Playlist),
            ..repeating_track
        };

        assert_eq!(settings_update(repeating_track, true).repeat, None);
        assert_eq!(settings_update(repeating_track, true).muted, Some(true));
        assert_eq!(
            settings_update(repeating_track, false).repeat,
            Some(RepeatMode::Track)
        );
        assert_eq!(
            settings_update(repeating_playlist, true).repeat,
            Some(RepeatMode::Playlist)
        );
    }
}
