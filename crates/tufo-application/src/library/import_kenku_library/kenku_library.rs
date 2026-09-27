//! Kenku FM's library as its data folder holds it.

use tufo_models::{
    library::{CollectionKind, SoundSettings, Volume},
    path::ImageFilePath,
    playback::{AudioFilePath, queue::RepeatMode},
};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct KenkuLibrary {
    /// In Kenku FM's order.
    pub collections: Vec<KenkuCollection>,
    pub player_settings: KenkuPlayerSettings,
}

/// Absent settings stay as they are.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct KenkuPlayerSettings {
    pub volume: Option<Volume>,
    pub muted: Option<bool>,
    pub shuffle: Option<bool>,
    pub repeat: Option<RepeatMode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KenkuCollection {
    pub kind: CollectionKind,
    /// Untrimmed.
    pub title: String,
    pub background: KenkuBackground,
    pub items: Vec<KenkuItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KenkuBackground {
    /// A picture Kenku FM ships, by file stem.
    Named(String),
    Image(ImageFilePath),
    /// A web address, or anything else.
    Unsupported,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KenkuItem {
    /// Untrimmed.
    pub title: String,
    pub source: KenkuSource,
    /// Tracks carry the defaults.
    pub sound: SoundSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KenkuSource {
    File(AudioFilePath),
    /// A web address, as Kenku FM stores it.
    Unsupported(String),
}
