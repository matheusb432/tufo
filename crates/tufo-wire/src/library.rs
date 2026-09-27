//! The process-neutral contracts for reading and changing the library.

use std::time::Duration;

use jiff::Timestamp;
use tufo_models::{
    library::{
        AudioFileId, BackgroundPreset, BuiltinTitle, CollectionEntryId, CollectionId,
        CollectionKind, EntryOrder, SoundIcon, SoundSettings, Title,
    },
    path::{FolderPath, ImageFilePath, LocalPath},
    playback::AudioFilePath,
};

/// Every collection with its entries in the collection's order. Each change to the library
/// publishes a new snapshot with a higher revision.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LibrarySnapshot {
    pub revision: u64,
    /// Playlists, then soundboards, each by title.
    pub collections: Vec<CollectionView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectionView {
    pub id: CollectionId,
    pub kind: CollectionKind,
    pub title: Title,
    pub builtin_title: Option<BuiltinTitle>,
    pub background: Background,
    pub order: EntryOrder,
    pub source: CollectionSource,
    pub entries: Vec<EntryView>,
}

/// What a collection's card and page show behind it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Background {
    /// A gradient derived from the collection's id.
    #[default]
    Gradient,
    Preset(BackgroundPreset),
    Image(ImageFilePath),
}

impl Background {
    /// The background a collection's stored columns describe; the database lets at most one of
    /// them be present.
    #[must_use]
    pub fn from_columns(preset: Option<BackgroundPreset>, image: Option<ImageFilePath>) -> Self {
        match (preset, image) {
            (_, Some(image)) => Self::Image(image),
            (Some(preset), None) => Self::Preset(preset),
            (None, None) => Self::Gradient,
        }
    }

    /// The preset and image columns that store this background.
    #[must_use]
    pub fn into_columns(self) -> (Option<BackgroundPreset>, Option<ImageFilePath>) {
        match self {
            Self::Gradient => (None, None),
            Self::Preset(preset) => (Some(preset), None),
            Self::Image(image) => (None, Some(image)),
        }
    }
}

/// Where a collection's entries come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectionSource {
    /// Whatever was added to it by hand.
    Manual,
    /// The audio files one folder holds directly.
    Linked { folder: FolderPath, missing: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct EntryView {
    pub id: CollectionEntryId,
    pub audio_file_id: AudioFileId,
    pub title: Title,
    pub builtin_title: Option<BuiltinTitle>,
    pub path: AudioFilePath,
    pub duration: Duration,
    /// When the file was added to Tufo.
    pub added_at: Timestamp,
    /// Whether Tufo last looked for the file and didn't find it.
    pub missing: bool,
    pub sound: SoundSettings,
    pub icon: SoundIcon,
}

/// Folders and files dropped on the playlists or soundboards grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPaths {
    pub kind: CollectionKind,
    pub paths: Vec<LocalPath>,
}

/// Files and folders added to one manual collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddFilesToCollection {
    pub collection_id: CollectionId,
    pub paths: Vec<LocalPath>,
}

/// What an import or addition changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub collections_created: usize,
    pub files_added: usize,
    /// Audio files Tufo couldn't read, left out.
    pub unreadable_files: Vec<AudioFilePath>,
    /// Dropped paths that are neither folders nor audio files.
    pub ignored_paths: Vec<LocalPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportKenkuLibrary {
    /// `None` for Kenku FM's default folder on the server's system.
    pub kenku_folder: Option<FolderPath>,
}

/// What importing Kenku FM's library changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KenkuImportReport {
    pub collections_imported: usize,
    pub files_imported: usize,
    /// Titles the library already showed.
    pub skipped_collections: Vec<Title>,
    /// Audio files that couldn't be found or read.
    pub unreadable_files: Vec<AudioFilePath>,
    /// Web addresses, which can't play.
    pub unsupported_sources: Vec<String>,
    /// The bot logs in with it after the server restarts.
    pub discord_bot_token_imported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCollection {
    pub kind: CollectionKind,
    pub title: Title,
    pub background: Background,
}

/// Changes to a collection; `None` leaves a property as it is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpdateCollection {
    pub title: Option<Title>,
    pub background: Option<Background>,
    pub order: Option<EntryOrder>,
}

/// Changes to an entry; `None` leaves a property as it is.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UpdateCollectionEntry {
    /// Retitles the audio file everywhere it appears.
    pub title: Option<Title>,
    pub sound: Option<SoundSettings>,
    pub icon: Option<SoundIcon>,
}
