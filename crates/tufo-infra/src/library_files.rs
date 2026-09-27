//! The library's view of the disk: folder scans, file stamps, and audio fingerprints.
//!
//! Every call runs on tokio's blocking pool, since directory walks and demuxing block.

mod inspect;
mod scan;

use std::{fs::Metadata, io, path::Path, time::UNIX_EPOCH};

use tufo_application::library::library_files::{
    AudioFacts, ClassifiedPath, FolderScan, InspectAudioError, LibraryFiles, ScanFolderError,
};
use tufo_models::{
    library::{FileIdentity, FileStamp},
    path::{FolderPath, LocalPath},
    playback::AudioFilePath,
};

/// Reads the library's folders and files from the local filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiskLibraryFiles;

impl LibraryFiles for DiskLibraryFiles {
    async fn classify_path(&self, path: LocalPath) -> Result<ClassifiedPath, io::Error> {
        run_blocking(move || classify_path(path.as_ref())).await?
    }

    async fn scan_folder(&self, root: FolderPath) -> Result<FolderScan, ScanFolderError> {
        run_blocking(move || scan::scan_folder(&root))
            .await
            .unwrap_or_else(|error| Err(ScanFolderError::Unreadable(error)))
    }

    async fn stat_file(&self, path: AudioFilePath) -> Result<Option<FileStamp>, io::Error> {
        run_blocking(move || stat_file(path.as_ref())).await?
    }

    async fn folder_identity(&self, path: FolderPath) -> Result<Option<FileIdentity>, io::Error> {
        run_blocking(move || folder_identity(path.as_ref())).await?
    }

    async fn inspect_audio(&self, path: AudioFilePath) -> Result<AudioFacts, InspectAudioError> {
        run_blocking(move || inspect::inspect_audio(path.as_ref()))
            .await
            .unwrap_or_else(|error| Err(InspectAudioError::Unreadable(error)))
    }
}

async fn run_blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, io::Error> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)
}

fn classify_path(path: &Path) -> Result<ClassifiedPath, io::Error> {
    let resolved = match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(ClassifiedPath::Other),
        Err(error) => return Err(error),
    };
    let metadata = std::fs::metadata(&resolved)?;
    if metadata.is_dir() {
        return Ok(
            FolderPath::try_new(resolved).map_or(ClassifiedPath::Other, ClassifiedPath::Folder)
        );
    }
    if metadata.is_file() && scan::has_audio_extension(&resolved) {
        return Ok(AudioFilePath::try_new(resolved)
            .map_or(ClassifiedPath::Other, ClassifiedPath::AudioFile));
    }
    Ok(ClassifiedPath::Other)
}

fn stat_file(path: &Path) -> Result<Option<FileStamp>, io::Error> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    file_stamp(path, &metadata).map(Some)
}

fn folder_identity(path: &Path) -> Result<Option<FileIdentity>, io::Error> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() {
        return Ok(None);
    }
    file_identity(path, &metadata).map(Some)
}

fn file_stamp(path: &Path, metadata: &Metadata) -> Result<FileStamp, io::Error> {
    Ok(FileStamp {
        identity: file_identity(path, metadata)?,
        size_bytes: metadata.len(),
        modified_ns: modified_ns(metadata),
    })
}

#[cfg(unix)]
fn file_identity(_path: &Path, metadata: &Metadata) -> Result<FileIdentity, io::Error> {
    use std::os::unix::fs::MetadataExt as _;

    identity_text(file_id::FileId::new_inode(metadata.dev(), metadata.ino()))
}

/// Windows needs an open handle for the file index, so this reads it from the path.
#[cfg(not(unix))]
fn file_identity(path: &Path, _metadata: &Metadata) -> Result<FileIdentity, io::Error> {
    identity_text(file_id::get_file_id(path)?)
}

fn identity_text(file_id: file_id::FileId) -> Result<FileIdentity, io::Error> {
    let text = match file_id {
        file_id::FileId::Inode {
            device_id,
            inode_number,
        } => format!("inode:{device_id}:{inode_number}"),
        file_id::FileId::LowRes {
            volume_serial_number,
            file_index,
        } => format!("index:{volume_serial_number}:{file_index}"),
        file_id::FileId::HighRes {
            volume_serial_number,
            file_id,
        } => format!("id:{volume_serial_number}:{file_id}"),
    };
    FileIdentity::try_new(text).map_err(io::Error::other)
}

/// Nanoseconds since the Unix epoch, negative before it, saturating at `i64`'s range. A
/// filesystem without modification times reads as 0.
fn modified_ns(metadata: &Metadata) -> i64 {
    let Ok(modified) = metadata.modified() else {
        return 0;
    };
    match modified.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_nanos()).map_or(i64::MIN, |ns| -ns),
    }
}
