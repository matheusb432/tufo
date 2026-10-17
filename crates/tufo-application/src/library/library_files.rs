//! The filesystem capability the library reads: folder scans, file stamps, and audio facts.

use std::{future::Future, time::Duration};

use tufo_models::{
    library::{AudioFingerprint, FileIdentity, FileSha256, FileStamp},
    path::{FolderPath, LocalPath},
    playback::AudioFilePath,
};
use tufo_wire::song_loops::SongWaveform;

/// Reads the folders and audio files the library points at. Nothing writes through it.
pub trait LibraryFiles: Send + Sync {
    /// Resolves symlinks and `..` in a dropped path and says what it is.
    fn classify_path(
        &self,
        path: LocalPath,
    ) -> impl Future<Output = Result<ClassifiedPath, std::io::Error>> + Send;

    /// Lists `root` and every folder under it, with the audio files each holds directly.
    /// Symlinks aren't followed, and entries whose names aren't UTF-8 are skipped.
    fn scan_folder(
        &self,
        root: FolderPath,
    ) -> impl Future<Output = Result<FolderScan, ScanFolderError>> + Send;

    /// Reads the stamp of one file, or `None` when nothing is at `path`.
    fn stat_file(
        &self,
        path: AudioFilePath,
    ) -> impl Future<Output = Result<Option<FileStamp>, std::io::Error>> + Send;

    /// Reads the file identity of the folder at `path`, or `None` when no folder is there.
    fn folder_identity(
        &self,
        path: FolderPath,
    ) -> impl Future<Output = Result<Option<FileIdentity>, std::io::Error>> + Send;

    /// Demuxes the file's audio packets, without decoding them, into its fingerprint and exact
    /// duration.
    fn inspect_audio(
        &self,
        path: AudioFilePath,
    ) -> impl Future<Output = Result<AudioFacts, InspectAudioError>> + Send;

    fn sha256_of_file(
        &self,
        path: AudioFilePath,
    ) -> impl Future<Output = Result<FileSha256, std::io::Error>> + Send;

    /// Decodes the whole file, which takes seconds for long files, into its fingerprint, the
    /// frame count playback decodes, and its peaks.
    fn analyze_audio(
        &self,
        path: AudioFilePath,
    ) -> impl Future<Output = Result<SongWaveform, InspectAudioError>> + Send;
}

/// What a dropped path turned out to be, at its resolved location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifiedPath {
    Folder(FolderPath),
    /// A file whose extension names an audio format.
    AudioFile(AudioFilePath),
    /// Anything else, including paths that don't exist.
    Other,
}

/// One library folder as the filesystem shows it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderScan {
    /// The scanned root first, then every folder under it.
    pub folders: Vec<ScannedFolder>,
    /// Whether the scan stopped at its file limit before seeing everything.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFolder {
    pub path: FolderPath,
    pub identity: FileIdentity,
    /// The files this folder holds directly whose extensions name an audio format.
    pub audio_files: Vec<ScannedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub path: AudioFilePath,
    pub stamp: FileStamp,
}

/// What reading a file's audio packets revealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFacts {
    pub fingerprint: AudioFingerprint,
    pub duration: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum ScanFolderError {
    #[error("the folder does not exist")]
    NotFound,
    #[error("the path is not a folder")]
    NotAFolder,
    #[error("the folder cannot be read")]
    Unreadable(#[source] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum InspectAudioError {
    #[error("the audio file cannot be read")]
    Unreadable(#[source] std::io::Error),
    #[error("the file holds no audio track that can be decoded")]
    Unsupported(#[source] anyhow::Error),
}
