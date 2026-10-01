//! Importing folders and files dropped on the playlists or soundboards grid.
//!
//! A dropped folder becomes a library folder that Tufo follows. Dropping a folder inside one it
//! already follows re-follows the collections under it that were deleted. Dropping a folder
//! that holds library folders of the same kind absorbs them, keeping their collections.
//! Loose audio files become one manual collection, titled after the first file's folder; dropped
//! as music, they also become song sources.

use jiff::Timestamp;
use toasty::Db;
use tufo_models::{
    library::{BuiltinTitle, Collection, CollectionKind, LibraryFolder, Title},
    path::FolderPath,
};
use tufo_wire::library::{Background, ImportPaths, ImportReport};

use super::{
    Library, LibraryStorageError,
    library_files::{ClassifiedPath, LibraryFiles},
    manual_files, store, sync_library,
};
use crate::error::ErrorMeta;

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum ImportPathsError {
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

pub async fn execute<F: LibraryFiles>(
    request: ImportPaths,
    library: &Library<F>,
) -> Result<ImportReport, ImportPathsError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let mut report = ImportReport::default();
    let mut loose_files = Vec::new();
    for path in request.paths {
        match library.files().classify_path(path.clone()).await {
            Ok(ClassifiedPath::Folder(folder)) => {
                follow_folder(&mut database, library.files(), request.kind, folder).await?;
            }
            Ok(ClassifiedPath::AudioFile(file)) => loose_files.push(file),
            Ok(ClassifiedPath::Other) | Err(_) => report.ignored_paths.push(path),
        }
    }
    let synced = sync_library::sync_while_changing(library).await?;
    report.collections_created = synced.collections_created;
    report.files_added = synced.files_added;
    report.unreadable_files = synced.unreadable_files;

    if let Some(first) = loose_files.first() {
        let folder_name = first
            .as_ref()
            .parent()
            .and_then(|folder| folder.file_name());
        let title = folder_name.map_or_else(
            || Title::from_name_lossy("Imported"),
            |name| Title::from_name_lossy(&name.to_string_lossy()),
        );
        let preset = store::background_preset_chooser(&mut database)
            .await?
            .choose();
        let collection = store::create_manual_collection(
            &mut database,
            request.kind,
            title,
            Background::Preset(preset),
            folder_name.is_none().then_some(BuiltinTitle::Imported),
        )
        .await?;
        let added = manual_files::add_files(library, &collection, loose_files).await?;
        report.collections_created += 1;
        report.files_added += added.files_added;
        report.unreadable_files.extend(added.unreadable);
    }
    library.publish().await?;
    Ok(report)
}

pub(super) async fn follow_folder(
    database: &mut Db,
    files: &impl LibraryFiles,
    kind: CollectionKind,
    folder: FolderPath,
) -> Result<(), LibraryStorageError> {
    let library_folders = LibraryFolder::all()
        .exec(database)
        .await?
        .into_iter()
        .filter(|library_folder| library_folder.kind == kind)
        .collect::<Vec<_>>();
    let enclosing = library_folders.iter().find(|library_folder| {
        folder
            .as_ref()
            .starts_with(library_folder.folder_path.as_ref())
    });
    if enclosing.is_some() {
        return refollow_collections_under(database, &folder, kind).await;
    }
    let Ok(Some(identity)) = files.folder_identity(folder.clone()).await else {
        return Ok(());
    };
    let created = toasty::create!(LibraryFolder {
        kind,
        folder_path: folder.clone(),
        folder_identity: identity,
        created_at: Timestamp::now(),
    })
    .exec(database)
    .await?;
    for absorbed in library_folders.iter().filter(|library_folder| {
        library_folder
            .folder_path
            .as_ref()
            .starts_with(folder.as_ref())
    }) {
        absorb_library_folder(database, absorbed, &created).await?;
    }
    refollow_collections_under(database, &folder, kind).await
}

/// Moves a nested library folder's collections to the folder that now holds it.
async fn absorb_library_folder(
    database: &mut Db,
    absorbed: &LibraryFolder,
    into: &LibraryFolder,
) -> Result<(), LibraryStorageError> {
    // Toasty 0.10 can't serialize a filter on an optional UUID column, so this filters in memory.
    let collections = Collection::all()
        .exec(database)
        .await?
        .into_iter()
        .filter(|collection| collection.library_folder_id == Some(absorbed.id));
    for collection in collections {
        Collection::update_by_id(collection.id)
            .library_folder_id(Some(into.id))
            .exec(database)
            .await?;
    }
    LibraryFolder::filter_by_id(absorbed.id)
        .delete()
        .exec(database)
        .await?;
    Ok(())
}

/// Un-hides the deleted linked collections of `kind` at or under `folder`.
async fn refollow_collections_under(
    database: &mut Db,
    folder: &FolderPath,
    kind: CollectionKind,
) -> Result<(), LibraryStorageError> {
    let hidden = Collection::all()
        .exec(database)
        .await?
        .into_iter()
        .filter(|collection| {
            collection.hidden_at.is_some()
                && collection.kind == kind
                && collection
                    .folder_path
                    .as_ref()
                    .is_some_and(|path| path.as_ref().starts_with(folder.as_ref()))
        });
    for collection in hidden {
        Collection::update_by_id(collection.id)
            .hidden_at(None::<Timestamp>)
            .exec(database)
            .await?;
    }
    Ok(())
}
