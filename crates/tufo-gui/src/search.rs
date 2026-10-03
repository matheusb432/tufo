//! The Ctrl+Shift+F palette: fuzzy-matches every track, sound, and collection.

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use tufo_client::library::{CollectionView, EntryView, LibrarySnapshot};
use tufo_models::library::{CollectionEntryId, CollectionId, CollectionKind};

use crate::i18n::tr;

const RESULTS_MAX: usize = 25;
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

struct Scorer {
    pattern: Pattern,
    matcher: Matcher,
    buffer: Vec<char>,
    threshold: u32,
}

impl Scorer {
    fn new(query: &str) -> Self {
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buffer = Vec::new();
        // Scored against itself, the query sets the best possible score. A match inside a word
        // scores about 70% of it, and letters scattered across a title about 55%, so the cut
        // keeps the first and drops the second.
        let threshold = pattern
            .score(Utf32Str::new(query, &mut buffer), &mut matcher)
            .map_or(0, |best| best * MATCH_SHARE_MIN_PERCENT / 100);
        Self {
            pattern,
            matcher,
            buffer,
            threshold,
        }
    }

    fn matches(&mut self, haystack: &str) -> Option<u32> {
        self.pattern
            .score(Utf32Str::new(haystack, &mut self.buffer), &mut self.matcher)
            .filter(|points| *points >= self.threshold)
    }
}

/// Results for `query`, best matches first, leaving out weak matches. An entry in two
/// collections appears once for each. A blank query finds nothing.
pub(crate) fn search(snapshot: &LibrarySnapshot, query: &str) -> Vec<PaletteResult> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let mut ranked = match query.split_once('/') {
        Some((collection_query, entry_query)) if !collection_query.trim().is_empty() => {
            rank_within_collections(snapshot, collection_query.trim(), entry_query.trim())
        }
        Some((_, entry_query)) if !entry_query.trim().is_empty() => {
            rank_everything(snapshot, entry_query.trim())
        }
        _ => rank_everything(snapshot, query),
    };
    ranked.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.title.cmp(&right.1.title))
    });
    ranked
        .into_iter()
        .take(RESULTS_MAX)
        .map(|(_, result)| result)
        .collect()
}

fn rank_everything(snapshot: &LibrarySnapshot, query: &str) -> Vec<(u32, PaletteResult)> {
    let mut scorer = Scorer::new(query);
    let mut ranked = Vec::new();
    for collection in &snapshot.collections {
        let collection_title = collection.title.as_str();
        if let Some(points) = scorer.matches(collection_title) {
            ranked.push((points, collection_result(collection, collection_title)));
        }
        for entry in collection.entries.iter().filter(|entry| !entry.missing) {
            let haystack = format!("{} {collection_title}", entry.title.as_str());
            if let Some(points) = scorer.matches(&haystack) {
                ranked.push((points, entry_result(collection, collection_title, entry)));
            }
        }
    }
    ranked
}

fn rank_within_collections(
    snapshot: &LibrarySnapshot,
    collection_query: &str,
    entry_query: &str,
) -> Vec<(u32, PaletteResult)> {
    let mut collection_scorer = Scorer::new(collection_query);
    let mut entry_scorer = (!entry_query.is_empty()).then(|| Scorer::new(entry_query));
    let mut ranked = Vec::new();
    for collection in &snapshot.collections {
        let collection_title = collection.title.as_str();
        let Some(collection_points) = collection_scorer.matches(collection_title) else {
            continue;
        };
        for entry in collection.entries.iter().filter(|entry| !entry.missing) {
            let points = match &mut entry_scorer {
                Some(scorer) => match scorer.matches(entry.title.as_str()) {
                    Some(points) => collection_points + points,
                    None => continue,
                },
                None => collection_points,
            };
            ranked.push((points, entry_result(collection, collection_title, entry)));
        }
    }
    ranked
}

fn collection_result(collection: &CollectionView, collection_title: &str) -> PaletteResult {
    let kind_label = match collection.kind {
        CollectionKind::Playlist => tr("Playlist"),
        CollectionKind::Soundboard => tr("Soundboard"),
    };
    PaletteResult {
        section: tr("Collections"),
        title: collection_title.to_owned(),
        subtitle: kind_label.to_owned(),
        target: PaletteTarget::Collection {
            kind: collection.kind,
            id: collection.id,
        },
    }
}

fn entry_result(
    collection: &CollectionView,
    collection_title: &str,
    entry: &EntryView,
) -> PaletteResult {
    let (section, target) = match collection.kind {
        CollectionKind::Playlist => (
            tr("Music"),
            PaletteTarget::Track {
                collection_id: collection.id,
                entry_id: entry.id,
            },
        ),
        CollectionKind::Soundboard => (tr("Sounds"), PaletteTarget::Sound { entry_id: entry.id }),
    };
    PaletteResult {
        section,
        title: entry.title.as_str().to_owned(),
        subtitle: collection_title.to_owned(),
        target,
    }
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
            saved_loop: None,
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
            ..LibrarySnapshot::default()
        }
    }

    #[test]
    fn the_best_match_wins_regardless_of_category() {
        let mut snapshot = library();
        snapshot.collections[0]
            .entries
            .push(entry(15, "Special Effects Ambience", false));
        snapshot
            .collections
            .push(collection(4, CollectionKind::Soundboard, "Effects", vec![]));

        let results = search(&snapshot, "Effects");

        assert_eq!(results[0].title, "Effects");
        assert!(matches!(
            results[0].target,
            PaletteTarget::Collection { .. }
        ));
    }

    #[test]
    fn a_collections_name_finds_its_tracks() {
        let results = search(&library(), "boss brawl");

        assert_eq!(results[0].title, "Tavern Brawl");
        assert!(matches!(results[0].target, PaletteTarget::Track { .. }));
    }

    #[test]
    fn a_slash_narrows_to_entries_of_a_named_collection() {
        let results = search(&library(), "boss/brawl");

        let titles = results
            .into_iter()
            .map(|result| result.title)
            .collect::<Vec<_>>();
        assert_eq!(titles, ["Tavern Brawl"]);
    }

    #[test]
    fn a_slash_with_nothing_after_it_lists_every_entry_of_the_collection() {
        let results = search(&library(), "boss/");

        let mut titles = results
            .into_iter()
            .map(|result| result.title)
            .collect::<Vec<_>>();
        titles.sort();
        assert_eq!(titles, ["Ancient Dragon", "Tavern Brawl"]);
    }

    #[test]
    fn nothing_before_the_slash_searches_everything_after_it() {
        let results = search(&library(), "/dragon");

        assert!(
            results
                .iter()
                .any(|result| result.title == "Ancient Dragon"),
            "{results:?}"
        );
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
        assert_eq!(search(&library(), "  "), Vec::<PaletteResult>::new());
    }
}
