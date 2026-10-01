//! Resolving file aliases, reading audio facts, and preparing transactional reference updates.

use std::collections::{HashMap, HashSet};

use futures_util::{StreamExt as _, stream};
use jiff::Timestamp;
use toasty::{Db, db::Executor};
use tufo_models::{
    kenku_import::{KenkuImport, KenkuImportState},
    library::{AudioFile, AudioFileId, FileIdentity, FileStamp, milliseconds_saturating},
    path::{FolderPath, LocalPath},
    playback::AudioFilePath,
};

use super::{KenkuCollection, KenkuSource, journal, plan};
use crate::library::{
    Library, LibraryStorageError,
    library_files::{ClassifiedPath, FolderScan, LibraryFiles},
    manual_files,
    reconcile::NewFile,
    store,
};

pub(super) struct PreparedFiles {
    pub(super) stored: Vec<AudioFile>,
    pub(super) known: HashMap<AudioFilePath, AudioFileId>,
    pub(super) readable: Vec<NewFile>,
    pub(super) unreadable: Vec<AudioFilePath>,
    existing: HashMap<AudioFilePath, AudioFileId>,
    resolved_paths: Vec<(AudioFileId, AudioFilePath)>,
}

pub(super) async fn prepare<F: LibraryFiles>(
    library: &Library<F>,
    database: &mut Db,
    paths: HashSet<AudioFilePath>,
    stored_files: Vec<AudioFile>,
    import: &mut KenkuImport,
) -> Result<PreparedFiles, LibraryStorageError> {
    let by_path = existing_paths(&stored_files);
    let mut by_identity = HashMap::<&FileIdentity, Vec<&AudioFile>>::new();
    for file in &stored_files {
        by_identity
            .entry(&file.file_identity)
            .or_default()
            .push(file);
    }
    let mut known = HashMap::new();
    let mut existing = HashMap::new();
    let mut resolved_paths = Vec::new();
    let mut to_read = Vec::new();
    for path in paths {
        let stamp = library.files().stat_file(path.clone()).await.ok().flatten();
        let previous = match by_path.get(&path_key(&path)) {
            Some(file) => Some(*file),
            None => resolve_alias(library.files(), &path, stamp.as_ref(), &by_identity).await,
        };
        if let Some(previous) = previous {
            existing.insert(path.clone(), previous.id);
            if previous.file_path != path {
                resolved_paths.push((previous.id, path.clone()));
            }
            if stamp
                .as_ref()
                .is_some_and(|stamp| previous.stamp() == *stamp)
            {
                known.insert(path, previous.id);
                continue;
            }
        }
        to_read.push(path);
    }
    to_read.sort();
    let chunk_size = to_read.len().div_ceil(20).max(64);
    let mut readable = Vec::new();
    let mut unreadable = Vec::new();
    for (index, chunk) in to_read.chunks(chunk_size).enumerate() {
        let read = manual_files::read_files(library.files(), chunk.to_vec()).await;
        readable.extend(read.readable);
        unreadable.extend(read.unreadable);
        let done = ((index + 1) * chunk_size).min(to_read.len());
        let progress = 30 + u32::try_from(done * 50 / to_read.len()).unwrap_or(50);
        journal::advance(
            database,
            &library.kenku_imports,
            import,
            KenkuImportState::Importing,
            progress,
        )
        .await?;
    }
    Ok(PreparedFiles {
        stored: stored_files,
        known,
        readable,
        unreadable,
        existing,
        resolved_paths,
    })
}

fn existing_paths(stored_files: &[AudioFile]) -> HashMap<std::path::PathBuf, &AudioFile> {
    let paths = stored_files
        .iter()
        .map(|file| (path_key(&file.file_path), file))
        .collect::<HashMap<_, _>>();
    #[cfg(windows)]
    let paths = {
        let mut paths = paths;
        // If both aliases already exist, use the canonical row without merging references.
        for file in stored_files {
            if matches!(file.file_path.as_ref().components().next(), Some(std::path::Component::Prefix(prefix)) if prefix.kind().is_verbatim())
            {
                paths.insert(path_key(&file.file_path), file);
            }
        }
        paths
    };
    paths
}

fn path_key(path: &AudioFilePath) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let text = path.as_ref().to_string_lossy();
        if let Some(tail) = text.strip_prefix(r"\\?\UNC\") {
            return std::path::PathBuf::from(format!(r"\\{tail}"));
        }
        if let Some(tail) = text.strip_prefix(r"\\?\") {
            return std::path::PathBuf::from(tail);
        }
    }
    path.as_ref().clone()
}

async fn resolve_alias<'a>(
    files: &impl LibraryFiles,
    path: &AudioFilePath,
    stamp: Option<&FileStamp>,
    by_identity: &HashMap<&FileIdentity, Vec<&'a AudioFile>>,
) -> Option<&'a AudioFile> {
    let candidates = by_identity.get(&stamp?.identity)?;
    for previous in candidates {
        let local = LocalPath::try_new(previous.file_path.as_ref().clone()).ok()?;
        if let Ok(ClassifiedPath::AudioFile(canonical)) = files.classify_path(local).await
            && &canonical == path
        {
            return Some(previous);
        }
    }
    None
}

pub(super) async fn apply(
    executor: &mut dyn Executor,
    files: &mut PreparedFiles,
    now: Timestamp,
) -> Result<(), LibraryStorageError> {
    for (id, path) in &files.resolved_paths {
        AudioFile::update_by_id(*id)
            .file_path(path.clone())
            .exec(executor)
            .await?;
    }
    for file in &files.readable {
        let id = match files.existing.get(&file.path) {
            Some(id) => {
                update_audio_file(executor, *id, file).await?;
                *id
            }
            None => {
                store::create_audio_file(executor, file, now, None)
                    .await?
                    .id
            }
        };
        files.known.insert(file.path.clone(), id);
    }
    let present_ids = files.known.values().copied().collect::<HashSet<_>>();
    for file in files
        .stored
        .iter()
        .filter(|file| file.missing_since.is_some() && present_ids.contains(&file.id))
    {
        AudioFile::update_by_id(file.id)
            .missing_since(None::<Timestamp>)
            .exec(executor)
            .await
            .map_err(LibraryStorageError::from)?;
    }
    Ok(())
}

pub(super) async fn normalize_paths(
    files: &impl LibraryFiles,
    collections: &mut [KenkuCollection],
) {
    let paths = collections
        .iter()
        .flat_map(|collection| collection.items.iter())
        .filter_map(|item| match &item.source {
            KenkuSource::File(path) => Some(path.clone()),
            KenkuSource::Unsupported(_) => None,
        })
        .collect::<HashSet<_>>();
    let resolved: HashMap<_, _> = stream::iter(paths)
        .map(|path| async move {
            let canonical = match LocalPath::try_new(path.as_ref().clone()) {
                Ok(local) => match files.classify_path(local).await {
                    Ok(ClassifiedPath::AudioFile(canonical)) => canonical,
                    _ => normalize_missing(files, &path)
                        .await
                        .unwrap_or_else(|| path.clone()),
                },
                Err(_) => path.clone(),
            };
            (path, canonical)
        })
        .buffer_unordered(16)
        .collect()
        .await;
    for collection in collections {
        for item in &mut collection.items {
            if let KenkuSource::File(path) = &mut item.source
                && let Some(canonical) = resolved.get(path)
            {
                path.clone_from(canonical);
            }
        }
    }
}

async fn normalize_missing(
    files: &impl LibraryFiles,
    path: &AudioFilePath,
) -> Option<AudioFilePath> {
    let parent = LocalPath::try_new(path.as_ref().parent()?.to_path_buf()).ok()?;
    let Ok(ClassifiedPath::Folder(parent)) = files.classify_path(parent).await else {
        return None;
    };
    AudioFilePath::try_new(parent.as_ref().join(path.as_ref().file_name()?)).ok()
}

pub(super) async fn scan_candidates(
    files: &impl LibraryFiles,
    collections: &[KenkuCollection],
) -> HashMap<FolderPath, FolderScan> {
    let mut scans = HashMap::new();
    for folder in collections
        .iter()
        .filter_map(plan::folder_candidate)
        .collect::<HashSet<_>>()
    {
        if let Ok(scan) = files.scan_folder(folder.clone()).await
            && !scan.truncated
        {
            scans.insert(folder, scan);
        }
    }
    scans
}

async fn update_audio_file(
    executor: &mut dyn Executor,
    id: AudioFileId,
    file: &NewFile,
) -> Result<(), LibraryStorageError> {
    AudioFile::update_by_id(id)
        .file_identity(file.stamp.identity.clone())
        .size_bytes(i64::try_from(file.stamp.size_bytes).unwrap_or(i64::MAX))
        .modified_ns(file.stamp.modified_ns)
        .fingerprint(file.facts.fingerprint.clone())
        .duration_ms(milliseconds_saturating(file.facts.duration))
        .missing_since(None::<Timestamp>)
        .exec(executor)
        .await?;
    Ok(())
}
