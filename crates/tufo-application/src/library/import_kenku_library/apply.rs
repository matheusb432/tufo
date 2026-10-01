//! Filesystem preparation followed by one transaction for collections and their journal result.

use std::collections::{HashMap, HashSet};

use anyhow::Context as _;
use jiff::Timestamp;
use toasty::{Db, db::Executor};
use toasty_core::driver::operation::TransactionMode;
use tufo_models::{
    kenku_import::{KenkuImport, KenkuImportState},
    library::{
        AudioFile, AudioFileId, Collection, CollectionEntry, CollectionId, CollectionKind,
        EntryOrderKey, FileIdentity, LibraryFolder, LibraryFolderId, SoundIcon, SoundSettings,
        Title,
    },
    path::FolderPath,
    playback::AudioFilePath,
};
use tufo_wire::library::{Background, KenkuCollectionChoice, KenkuImportReport};

use super::{
    ImportKenkuLibraryError, KenkuCollection, conflicts, files,
    journal::{self, Details, ImportedCollection},
    plan::{self, PlanInput, PlannedCollection},
};
use crate::library::{
    Library, LibraryStorageError,
    library_files::{FolderScan, LibraryFiles},
    store,
};

pub(super) async fn collections<F: LibraryFiles>(
    incoming: Vec<KenkuCollection>,
    choices: &[KenkuCollectionChoice],
    library: &Library<F>,
    import: &mut KenkuImport,
    details: &mut Details,
) -> Result<Option<KenkuImportReport>, ImportKenkuLibraryError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let PreparedPlan {
        mut plan,
        existing,
        entries,
        stored_files,
        mut scans,
    } = prepare_plan(library, &mut database, incoming).await?;
    let fresh_conflicts = conflicts::collect(
        &plan.collections,
        &existing,
        &entries,
        &stored_files,
        details,
    )?;
    let needs_choices = !fresh_conflicts.is_empty() && choices.is_empty();
    let choices_changed = !choices.is_empty() && fresh_conflicts != details.conflicts;
    if needs_choices || choices_changed {
        // Re-offer choices when a target changes or disappears while the dialog is open.
        details.conflicts = fresh_conflicts;
        details.choices.clear();
        import.details = details.encode()?;
        journal::save(&mut database, import).await?;
        journal::advance(
            &mut database,
            &library.kenku_imports,
            import,
            KenkuImportState::AwaitingChoices,
            15,
        )
        .await?;
        return Ok(None);
    }
    let mut report = KenkuImportReport {
        unsupported_sources: std::mem::take(&mut plan.unsupported_sources),
        discord_bot_token_imported: details.report.report()?.discord_bot_token_imported,
        ..KenkuImportReport::default()
    };
    let destinations = select_destinations(
        &mut plan.collections,
        &existing,
        details,
        choices,
        &mut report,
    )?;
    let selected = plan
        .collections
        .iter()
        .filter(|planned| !conflicts::keep_existing(planned, details, choices))
        .collect::<Vec<_>>();
    journal::advance(
        &mut database,
        &library.kenku_imports,
        import,
        KenkuImportState::Importing,
        20,
    )
    .await?;
    let roots = prepare_roots(library.files(), &mut database, &selected, &mut scans).await?;
    let paths = import_paths(&selected, &roots, &existing);
    journal::advance(
        &mut database,
        &library.kenku_imports,
        import,
        KenkuImportState::Importing,
        30,
    )
    .await?;
    let mut files = files::prepare(library, &mut database, paths, stored_files, import).await?;
    report.unreadable_files.append(&mut files.unreadable);
    journal::advance(
        &mut database,
        &library.kenku_imports,
        import,
        KenkuImportState::Importing,
        85,
    )
    .await?;
    let write = ImportWrite {
        plan,
        existing,
        destinations,
        roots,
        files,
        report,
    };
    let report = commit(library, &mut database, write, choices, import, details).await?;
    Ok(Some(report))
}

struct ImportWrite {
    plan: plan::ImportPlan,
    existing: Vec<Collection>,
    destinations: Destinations,
    roots: Vec<Root>,
    files: files::PreparedFiles,
    report: KenkuImportReport,
}

async fn commit<F: LibraryFiles>(
    library: &Library<F>,
    database: &mut Db,
    mut write: ImportWrite,
    choices: &[KenkuCollectionChoice],
    import: &mut KenkuImport,
    details: &mut Details,
) -> Result<KenkuImportReport, LibraryStorageError> {
    let mut presets = store::background_preset_chooser(database).await?;
    let now = Timestamp::now();
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await?;
    for root in &write.roots {
        apply_root(&mut transaction, root, &write.existing, now).await?;
    }
    files::apply(&mut transaction, &mut write.files, now).await?;
    let imported = apply_collections(&mut transaction, &mut write, details, choices, now).await?;
    add_descendants(
        &mut transaction,
        &write.roots,
        &write.files.known,
        &mut presets,
        now,
    )
    .await?;
    if write.destinations.values().any(|(_, overwrite)| *overwrite) {
        store::delete_unkept_files(&mut transaction).await?;
    }
    write.report.unreadable_files.sort();
    write.report.unreadable_files.dedup();
    details.collections = imported;
    details.conflicts.clear();
    details.choices.clear();
    details.report = (&write.report).into();
    import.details = details.encode()?;
    import.progress = tufo_models::kenku_import::ImportProgress::try_new(90)
        .map_err(|error| LibraryStorageError(anyhow::anyhow!(error)))?;
    journal::save(&mut transaction, import).await?;
    transaction.commit().await?;
    library.kenku_imports.publish(import)?;
    library.publish().await?;
    Ok(write.report)
}

struct PreparedPlan {
    plan: plan::ImportPlan,
    existing: Vec<Collection>,
    entries: Vec<CollectionEntry>,
    stored_files: Vec<AudioFile>,
    scans: HashMap<FolderPath, FolderScan>,
}

async fn prepare_plan<F: LibraryFiles>(
    library: &Library<F>,
    database: &mut Db,
    mut incoming: Vec<KenkuCollection>,
) -> Result<PreparedPlan, LibraryStorageError> {
    files::normalize_paths(library.files(), &mut incoming).await;
    let existing = Collection::all()
        .exec(database)
        .await
        .map_err(LibraryStorageError::from)?;
    let entries = CollectionEntry::all()
        .exec(database)
        .await
        .map_err(LibraryStorageError::from)?;
    let stored_files = AudioFile::all()
        .exec(database)
        .await
        .map_err(LibraryStorageError::from)?;
    let scans = files::scan_candidates(library.files(), &incoming).await;
    let folder_audio_files = scans
        .iter()
        .filter_map(|(path, scan)| {
            let root = scan.folders.iter().find(|folder| &folder.path == path)?;
            Some((
                path.clone(),
                root.audio_files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect(),
            ))
        })
        .collect();
    let plan = plan::plan(PlanInput {
        collections: incoming,
        shown_presets: existing
            .iter()
            .filter(|collection| collection.hidden_at.is_none())
            .filter_map(|collection| collection.background_preset)
            .collect(),
        folder_audio_files,
        rng: fastrand::Rng::new(),
    });
    Ok(PreparedPlan {
        plan,
        existing,
        entries,
        stored_files,
        scans,
    })
}

type Destinations = HashMap<(CollectionKind, String), (CollectionId, bool)>;

fn select_destinations(
    planned: &mut [PlannedCollection],
    existing: &[Collection],
    details: &Details,
    choices: &[KenkuCollectionChoice],
    report: &mut KenkuImportReport,
) -> Result<Destinations, LibraryStorageError> {
    let mut destinations = HashMap::new();
    for planned in planned {
        if conflicts::keep_existing(planned, details, choices) {
            report.skipped_collections.push(planned.title.clone());
            continue;
        }
        let prior = details.collections.iter().find(|mapping| {
            mapping.kind == planned.kind.into() && mapping.source_id == planned.source_id
        });
        let previous = prior
            .map(ImportedCollection::destination)
            .transpose()?
            .flatten()
            .and_then(|id| {
                existing
                    .iter()
                    .find(|collection| collection.id == id && collection.hidden_at.is_none())
            });
        let overwrite = conflicts::overwrite_target(planned, details, choices)?;
        if let Some(previous) = previous {
            // A retry fills the same collection, even if restored files now make a manual
            // collection eligible for automatic folder following.
            planned.folder.clone_from(&previous.folder_path);
            destinations.insert(
                (planned.kind, planned.source_id.clone()),
                (previous.id, false),
            );
        } else if let Some(id) = overwrite {
            destinations.insert((planned.kind, planned.source_id.clone()), (id, true));
        }
    }
    Ok(destinations)
}

fn import_paths(
    selected: &[&PlannedCollection],
    roots: &[Root],
    existing: &[Collection],
) -> HashSet<AudioFilePath> {
    let claimed = existing
        .iter()
        .filter_map(|collection| Some((collection.kind, collection.folder_path.as_ref()?)))
        .collect::<HashSet<_>>();
    let mut paths = selected
        .iter()
        .flat_map(|planned| planned.files.iter().map(|file| file.path.clone()))
        .collect::<HashSet<_>>();
    // New followed roots also expose their subfolders. Existing collections (including hidden
    // ones) are left alone; only genuinely new folders need their files prepared here.
    for root in roots {
        for folder in &root.scan.folders {
            if !claimed.contains(&(root.kind, &folder.path)) {
                paths.extend(folder.audio_files.iter().map(|file| file.path.clone()));
            }
        }
    }
    paths
}

async fn apply_collections(
    executor: &mut dyn Executor,
    write: &mut ImportWrite,
    details: &Details,
    choices: &[KenkuCollectionChoice],
    now: Timestamp,
) -> Result<Vec<ImportedCollection>, LibraryStorageError> {
    let mut imported = Vec::new();
    for planned in &write.plan.collections {
        if conflicts::keep_existing(planned, details, choices) {
            imported.push(ImportedCollection {
                source_id: planned.source_id.clone(),
                kind: planned.kind.into(),
                destination: None,
            });
            continue;
        }
        let prior = write
            .destinations
            .get(&(planned.kind, planned.source_id.clone()))
            .copied();
        let collection =
            destination(executor, planned, prior, &write.existing, &write.roots, now).await?;
        add_entries(
            executor,
            &collection,
            planned.files.iter().filter_map(|file| {
                write
                    .files
                    .known
                    .get(&file.path)
                    .map(|id| (*id, SoundSettings::default()))
            }),
            now,
        )
        .await?;
        apply_file_settings(executor, collection.id, planned, &write.files.known).await?;
        write.report.collections_imported += 1;
        write.report.files_imported += planned
            .files
            .iter()
            .filter(|file| write.files.known.contains_key(&file.path))
            .map(|file| &file.path)
            .collect::<HashSet<_>>()
            .len();
        imported.push(ImportedCollection {
            source_id: planned.source_id.clone(),
            kind: planned.kind.into(),
            destination: Some(collection.id.to_string()),
        });
    }
    Ok(imported)
}

async fn add_descendants(
    executor: &mut dyn Executor,
    roots: &[Root],
    known: &HashMap<AudioFilePath, AudioFileId>,
    presets: &mut tufo_models::library::BackgroundPresetChooser,
    now: Timestamp,
) -> Result<(), LibraryStorageError> {
    // The incoming root is now claimed. Populate new descendant collections without retitling,
    // restoring or replacing any other collection that already points at those folders.
    let mut claimed = Collection::all()
        .exec(executor)
        .await
        .map_err(LibraryStorageError::from)?
        .into_iter()
        .filter_map(|collection| Some((collection.kind, collection.folder_path?)))
        .collect::<HashSet<_>>();
    for root in roots {
        for folder in &root.scan.folders {
            let files = folder
                .audio_files
                .iter()
                .filter_map(|file| known.get(&file.path).copied())
                .collect::<Vec<_>>();
            if claimed.contains(&(root.kind, folder.path.clone())) || files.is_empty() {
                continue;
            }
            let created = create_linked(
                executor,
                root,
                &folder.path,
                folder.identity.clone(),
                Title::from_name_lossy(
                    &folder
                        .path
                        .as_ref()
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                ),
                Background::Preset(presets.choose()),
                now,
            )
            .await?;
            add_entries(
                executor,
                &created,
                files.into_iter().map(|id| (id, SoundSettings::default())),
                now,
            )
            .await?;
            claimed.insert((root.kind, folder.path.clone()));
        }
    }
    Ok(())
}

struct Root {
    id: LibraryFolderId,
    kind: CollectionKind,
    path: FolderPath,
    identity: FileIdentity,
    new: bool,
    absorbed: Vec<LibraryFolderId>,
    scan: FolderScan,
}

async fn prepare_roots(
    files: &impl LibraryFiles,
    database: &mut Db,
    selected: &[&PlannedCollection],
    scans: &mut HashMap<FolderPath, FolderScan>,
) -> Result<Vec<Root>, LibraryStorageError> {
    let stored = LibraryFolder::all().exec(database).await?;
    let mut candidates = selected
        .iter()
        .filter_map(|planned| planned.folder.clone().map(|folder| (planned.kind, folder)))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(_, folder)| folder.as_ref().components().count());
    candidates.dedup();
    let mut roots: Vec<Root> = Vec::new();
    for (kind, folder) in candidates {
        if roots
            .iter()
            .any(|root| root.kind == kind && folder.as_ref().starts_with(root.path.as_ref()))
        {
            continue;
        }
        let enclosing = stored
            .iter()
            .filter(|root| {
                root.kind == kind && folder.as_ref().starts_with(root.folder_path.as_ref())
            })
            .max_by_key(|root| root.folder_path.as_ref().components().count());
        let path = enclosing.map_or_else(|| folder.clone(), |root| root.folder_path.clone());
        let scan = match scans.remove(&path) {
            Some(scan) => scan,
            None => match files.scan_folder(path.clone()).await {
                Ok(scan) if !scan.truncated => scan,
                _ => continue,
            },
        };
        let Some(identity) = scan
            .folders
            .iter()
            .find(|folder| folder.path == path)
            .map(|folder| folder.identity.clone())
        else {
            continue;
        };
        roots.push(Root {
            id: enclosing.map_or_else(
                || LibraryFolderId::from_uuid(uuid::Uuid::new_v4()),
                |root| root.id,
            ),
            kind,
            path: path.clone(),
            identity,
            new: enclosing.is_none(),
            absorbed: if enclosing.is_none() {
                stored
                    .iter()
                    .filter(|root| {
                        root.kind == kind && root.folder_path.as_ref().starts_with(path.as_ref())
                    })
                    .map(|root| root.id)
                    .collect()
            } else {
                Vec::new()
            },
            scan,
        });
    }
    Ok(roots)
}

async fn apply_root(
    executor: &mut dyn Executor,
    root: &Root,
    existing: &[Collection],
    now: Timestamp,
) -> Result<(), LibraryStorageError> {
    if !root.new {
        return Ok(());
    }
    toasty::create!(LibraryFolder {
        id: root.id,
        kind: root.kind,
        folder_path: root.path.clone(),
        folder_identity: root.identity.clone(),
        created_at: now
    })
    .exec(executor)
    .await?;
    for absorbed in &root.absorbed {
        for collection in existing
            .iter()
            .filter(|collection| collection.library_folder_id == Some(*absorbed))
        {
            Collection::update_by_id(collection.id)
                .library_folder_id(Some(root.id))
                .exec(executor)
                .await?;
        }
        LibraryFolder::filter_by_id(*absorbed)
            .delete()
            .exec(executor)
            .await?;
    }
    Ok(())
}

async fn destination(
    executor: &mut dyn Executor,
    planned: &PlannedCollection,
    prior: Option<(CollectionId, bool)>,
    existing: &[Collection],
    roots: &[Root],
    now: Timestamp,
) -> Result<Collection, LibraryStorageError> {
    let linked = planned.folder.as_ref().and_then(|folder| {
        roots
            .iter()
            .find(|root| {
                root.kind == planned.kind && folder.as_ref().starts_with(root.path.as_ref())
            })
            .and_then(|root| {
                root.scan
                    .folders
                    .iter()
                    .find(|scanned| &scanned.path == folder)
                    .map(|scanned| (root, scanned))
            })
    });
    if let Some((id, overwrite)) = prior {
        let previous = existing
            .iter()
            .find(|collection| collection.id == id)
            .context("the chosen collection disappeared")?;
        if !overwrite {
            return Ok(Collection::filter_by_id(id).get(executor).await?);
        }
        CollectionEntry::filter_by_collection_id(id)
            .delete()
            .exec(executor)
            .await?;
        let replaced_folder = previous.library_folder_id.is_some()
            && linked.map(|(_, folder)| &folder.path) != previous.folder_path.as_ref();
        if replaced_folder {
            // Leave a tombstone for the old followed folder so the watcher cannot recreate it
            // after replacing this collection with hand-picked Kenku entries.
            Collection::update_by_id(id)
                .hidden_at(Some(now))
                .exec(executor)
                .await?;
        } else {
            let (preset, image) = planned.background.clone().into_columns();
            let mut update = Collection::update_by_id(id)
                .title(planned.title.clone())
                .builtin_title(None::<tufo_models::library::BuiltinTitle>)
                .background_preset(preset)
                .background_image(image)
                .hidden_at(None::<Timestamp>);
            if let Some((root, folder)) = linked {
                update = update
                    .library_folder_id(Some(root.id))
                    .folder_path(Some(folder.path.clone()))
                    .folder_identity(Some(folder.identity.clone()))
                    .folder_missing_since(None::<Timestamp>);
            }
            update.exec(executor).await?;
            return Ok(Collection::filter_by_id(id).get(executor).await?);
        }
    }
    if let Some((root, folder)) = linked {
        create_linked(
            executor,
            root,
            &folder.path,
            folder.identity.clone(),
            planned.title.clone(),
            planned.background.clone(),
            now,
        )
        .await
    } else {
        store::create_manual_collection(
            executor,
            planned.kind,
            planned.title.clone(),
            planned.background.clone(),
            None,
        )
        .await
    }
}

async fn create_linked(
    executor: &mut dyn Executor,
    root: &Root,
    folder: &FolderPath,
    identity: FileIdentity,
    title: Title,
    background: Background,
    now: Timestamp,
) -> Result<Collection, LibraryStorageError> {
    let (preset, image) = background.into_columns();
    Ok(toasty::create!(Collection {
        kind: root.kind,
        title,
        background_preset: preset,
        background_image: image,
        order_key: EntryOrderKey::default(),
        order_descending: false,
        library_folder_id: Some(root.id),
        folder_path: Some(folder.clone()),
        folder_identity: Some(identity),
        folder_missing_since: None,
        hidden_at: None,
        created_at: now
    })
    .exec(executor)
    .await?)
}

async fn add_entries(
    executor: &mut dyn Executor,
    collection: &Collection,
    files: impl Iterator<Item = (AudioFileId, SoundSettings)> + Send,
    now: Timestamp,
) -> Result<(), LibraryStorageError> {
    let mut listed = CollectionEntry::filter_by_collection_id(collection.id)
        .exec(executor)
        .await?
        .into_iter()
        .map(|entry| entry.audio_file_id)
        .collect::<HashSet<_>>();
    for (id, sound) in files {
        if collection.kind == CollectionKind::Playlist && collection.library_folder_id.is_none() {
            AudioFile::update_by_id(id)
                .song_source(true)
                .exec(executor)
                .await?;
        }
        if listed.insert(id) {
            store::create_entry(
                executor,
                collection.id,
                id,
                now,
                sound,
                SoundIcon::default(),
            )
            .await?;
        }
    }
    Ok(())
}

async fn apply_file_settings(
    executor: &mut dyn Executor,
    collection_id: CollectionId,
    planned: &PlannedCollection,
    known: &HashMap<AudioFilePath, AudioFileId>,
) -> Result<(), LibraryStorageError> {
    let entries = CollectionEntry::filter_by_collection_id(collection_id)
        .exec(executor)
        .await?
        .into_iter()
        .map(|entry| (entry.audio_file_id, entry.id))
        .collect::<HashMap<_, _>>();
    for file in &planned.files {
        if let (Some(title), Some(id)) = (&file.typed_title, known.get(&file.path)) {
            AudioFile::update_by_id(*id)
                .title(title.clone())
                .exec(executor)
                .await?;
        }
        if file.sound != SoundSettings::default()
            && let Some(entry_id) = known.get(&file.path).and_then(|id| entries.get(id))
        {
            CollectionEntry::update_by_id(*entry_id)
                .looping(file.sound.looping)
                .volume(file.sound.volume)
                .fade_in(file.sound.fade_in)
                .fade_out(file.sound.fade_out)
                .exec(executor)
                .await?;
        }
    }
    Ok(())
}
