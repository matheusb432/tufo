//! The Ctrl+Shift+F palette: fuzzy-matches every track, sound, and collection.

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use tufo_client::library::LibrarySnapshot;
use tufo_models::library::{CollectionEntryId, CollectionId, CollectionKind};

use crate::i18n::tr;

/// Each section shows at most this many results.
const SECTION_RESULTS_MAX: usize = 25;
/// A result must score at least this share of a perfect match.
const MATCH_SHARE_MIN_PERCENT: u32 = 65;

/// What activating a result does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteTarget {
    /// Plays the track within its playlist.
    Track {
        collection_id: CollectionId,
        entry_id: CollectionEntryId,
    },
    /// Fires the sound.
    Sound { entry_id: CollectionEntryId },
    /// Opens the collection's page.
    Collection {
        kind: CollectionKind,
        id: CollectionId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaletteResult {
    pub(crate) section: &'static str,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) target: PaletteTarget,
}

/// Results for `query`, grouped as Music, Sounds, then Collections, best matches first, leaving
/// out weak matches. An entry in two collections appears once for each. A blank query finds
/// nothing.
pub(crate) fn search(snapshot: &LibrarySnapshot, query: &str) -> Vec<PaletteResult> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buffer = Vec::new();
    // Scored against itself, the query sets the best possible score. A match inside a word
    // scores about 70% of it, and letters scattered across a title about 55%, so the cut keeps
    // the first and drops the second.
    let threshold = pattern
        .score(Utf32Str::new(query, &mut buffer), &mut matcher)
        .map_or(0, |best| best * MATCH_SHARE_MIN_PERCENT / 100);
    let mut score =
        |haystack: &str| pattern.score(Utf32Str::new(haystack, &mut buffer), &mut matcher);

    let mut music = Vec::new();
    let mut sounds = Vec::new();
    let mut collections = Vec::new();
    for collection in &snapshot.collections {
        let collection_title = collection.title.as_str();
        let kind_label = match collection.kind {
            CollectionKind::Playlist => tr("Playlist"),
            CollectionKind::Soundboard => tr("Soundboard"),
        };
        if let Some(points) = score(collection_title) {
            collections.push((
                points,
                PaletteResult {
                    section: tr("Collections"),
                    title: collection_title.to_owned(),
                    subtitle: kind_label.to_owned(),
                    target: PaletteTarget::Collection {
                        kind: collection.kind,
                        id: collection.id,
                    },
                },
            ));
        }
        for entry in collection.entries.iter().filter(|entry| !entry.missing) {
            let haystack = format!("{} {collection_title}", entry.title.as_str());
            let Some(points) = score(&haystack) else {
                continue;
            };
            let (section, target, bucket) = match collection.kind {
                CollectionKind::Playlist => (
                    tr("Music"),
                    PaletteTarget::Track {
                        collection_id: collection.id,
                        entry_id: entry.id,
                    },
                    &mut music,
                ),
                CollectionKind::Soundboard => (
                    tr("Sounds"),
                    PaletteTarget::Sound { entry_id: entry.id },
                    &mut sounds,
                ),
            };
            bucket.push((
                points,
                PaletteResult {
                    section,
                    title: entry.title.as_str().to_owned(),
                    subtitle: collection_title.to_owned(),
                    target,
                },
            ));
        }
    }
    [music, sounds, collections]
        .into_iter()
        .map(|section| {
            section
                .into_iter()
                .filter(|(points, _)| *points >= threshold)
                .collect::<Vec<_>>()
        })
        .flat_map(|mut section| {
            section.sort_by(|left, right| {
                right
                    .0
                    .cmp(&left.0)
                    .then_with(|| left.1.title.cmp(&right.1.title))
            });
            section
                .into_iter()
                .take(SECTION_RESULTS_MAX)
                .map(|(_, result)| result)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use tufo_client::library::{Background, CollectionSource, CollectionView, EntryView};
    use tufo_models::{
        library::{AudioFileId, EntryOrder, SoundIcon, SoundSettings, Title},
        playback::AudioFilePath,
    };

    use super::*;

    fn entry(seed: u128, title: &str, missing: bool) -> EntryView {
        EntryView {
            builtin_title: None,
            id: CollectionEntryId::from_uuid(uuid::Uuid::from_u128(seed)),
            audio_file_id: AudioFileId::from_uuid(uuid::Uuid::from_u128(seed)),
            title: Title::try_new(title.to_owned()).unwrap(),
            path: AudioFilePath::try_new(std::env::temp_dir().join(PathBuf::from(title))).unwrap(),
            duration: Duration::from_secs(60),
            added_at: jiff::Timestamp::UNIX_EPOCH,
            missing,
            sound: SoundSettings::default(),
            icon: SoundIcon::default(),
        }
    }

    fn collection(
        seed: u128,
        kind: CollectionKind,
        title: &str,
        entries: Vec<EntryView>,
    ) -> CollectionView {
        CollectionView {
            builtin_title: None,
            id: CollectionId::from_uuid(uuid::Uuid::from_u128(seed)),
            kind,
            title: Title::try_new(title.to_owned()).unwrap(),
            background: Background::Gradient,
            order: EntryOrder::default(),
            source: CollectionSource::Manual,
            entries,
        }
    }

    fn library() -> LibrarySnapshot {
        LibrarySnapshot {
            revision: 1,
            collections: vec![
                collection(
                    1,
                    CollectionKind::Playlist,
                    "boss",
                    vec![
                        entry(10, "Ancient Dragon", false),
                        entry(11, "Tavern Brawl", false),
                        entry(12, "Dragon (missing)", true),
                    ],
                ),
                collection(
                    2,
                    CollectionKind::Soundboard,
                    "Monsters",
                    vec![entry(13, "Dragon roar", false)],
                ),
                collection(3, CollectionKind::Playlist, "Dragons", vec![]),
            ],
        }
    }

    #[test]
    fn results_group_music_then_sounds_then_collections() {
        let results = search(&library(), "dragon");

        let found = results
            .iter()
            .map(|result| (result.section, result.title.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            found,
            [
                ("Music", "Ancient Dragon"),
                ("Sounds", "Dragon roar"),
                ("Collections", "Dragons"),
            ]
        );
    }

    #[test]
    fn a_collections_name_finds_its_tracks() {
        let results = search(&library(), "boss brawl");

        assert_eq!(results[0].title, "Tavern Brawl");
        assert!(matches!(results[0].target, PaletteTarget::Track { .. }));
    }

    #[test]
    fn letters_scattered_across_a_title_do_not_match_a_word() {
        let mut snapshot = library();
        snapshot.collections[0]
            .entries
            .push(entry(14, "[dark]-ruin-gate dungeons", false));

        let titles = search(&snapshot, "dragon")
            .into_iter()
            .map(|result| result.title)
            .collect::<Vec<_>>();

        assert!(
            !titles.contains(&"[dark]-ruin-gate dungeons".to_owned()),
            "{titles:?}"
        );
    }

    #[test]
    fn a_blank_query_finds_nothing() {
        assert!(search(&library(), "  ").is_empty());
    }
}
