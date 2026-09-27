//! Walking a library folder for the audio files each folder holds directly.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use tufo_application::library::library_files::{
    FolderScan, ScanFolderError, ScannedFile, ScannedFolder,
};
use tufo_models::{
    path::FolderPath,
    playback::{AUDIO_FILE_EXTENSIONS, AudioFilePath},
};

use super::{file_identity, file_stamp};

/// A scan stops after this many audio files and reports itself truncated.
const SCAN_AUDIO_FILES_MAX: usize = 100_000;
/// A scan stops after this many folders and reports itself truncated.
const SCAN_FOLDERS_MAX: usize = 20_000;

pub(super) fn scan_folder(root: &FolderPath) -> Result<FolderScan, ScanFolderError> {
    let metadata = fs::symlink_metadata(root.as_ref()).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => ScanFolderError::NotFound,
        _ => ScanFolderError::Unreadable(error),
    })?;
    if !metadata.is_dir() {
        return Err(ScanFolderError::NotAFolder);
    }
    let mut walk = Walk::default();
    let root_folder = walk
        .read_folder(root.as_ref())
        .map_err(ScanFolderError::Unreadable)?;
    walk.folders.push(root_folder);
    while let Some(path) = walk.pending.pop() {
        if walk.folders.len() >= SCAN_FOLDERS_MAX || walk.audio_file_count >= SCAN_AUDIO_FILES_MAX {
            walk.truncated = true;
            break;
        }
        match walk.read_folder(&path) {
            Ok(folder) => walk.folders.push(folder),
            Err(error) => {
                // An unreadable subfolder hides its files, so nothing under it may be judged
                // missing.
                tracing::warn!(path = %path.display(), %error, "skipping an unreadable folder");
                walk.truncated = true;
            }
        }
    }
    walk.folders
        .sort_by(|left, right| left.path.as_ref().cmp(right.path.as_ref()));
    Ok(FolderScan {
        folders: walk.folders,
        truncated: walk.truncated,
    })
}

#[derive(Default)]
struct Walk {
    folders: Vec<ScannedFolder>,
    pending: Vec<PathBuf>,
    audio_file_count: usize,
    truncated: bool,
}

impl Walk {
    /// Reads one folder: queues its subfolders and stamps its audio files. Symlinks and names
    /// that aren't UTF-8 are skipped.
    fn read_folder(&mut self, path: &Path) -> io::Result<ScannedFolder> {
        let metadata = fs::symlink_metadata(path)?;
        let folder_path = FolderPath::try_new(path.to_path_buf())
            .map_err(|_| io::Error::other("the folder path is not absolute UTF-8"))?;
        let mut audio_files = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let entry_path = entry.path();
            if file_type.is_symlink() || entry_path.to_str().is_none() {
                continue;
            }
            if file_type.is_dir() {
                self.pending.push(entry_path);
                continue;
            }
            if !file_type.is_file() || !has_audio_extension(&entry_path) {
                continue;
            }
            if self.audio_file_count >= SCAN_AUDIO_FILES_MAX {
                self.truncated = true;
                break;
            }
            if let Some(file) = scanned_file(entry_path, &entry.metadata()?)? {
                audio_files.push(file);
                self.audio_file_count += 1;
            }
        }
        audio_files.sort_by(|left, right| left.path.as_ref().cmp(right.path.as_ref()));
        Ok(ScannedFolder {
            identity: file_identity(path, &metadata)?,
            path: folder_path,
            audio_files,
        })
    }
}

fn scanned_file(path: PathBuf, metadata: &fs::Metadata) -> io::Result<Option<ScannedFile>> {
    let stamp = file_stamp(&path, metadata)?;
    Ok(AudioFilePath::try_new(path)
        .ok()
        .map(|path| ScannedFile { path, stamp }))
}

pub(super) fn has_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            AUDIO_FILE_EXTENSIONS
                .iter()
                .any(|audio| audio.eq_ignore_ascii_case(extension))
        })
}
