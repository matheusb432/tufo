//! The Ctrl+Shift+F palette: finds tracks, sounds, and collections by the words in their titles.
//!
//! Matching and ranking follow Meilisearch's defaults. Every query word must match a title word
//! whole, as its beginning, or within a few typos. Results rank by typos, then by whether the words
//! are in the result's own title rather than its collection's, then by how close together and in
//! what order they appear, then by exactness.

use std::collections::HashMap;

use tufo_client::library::LibrarySnapshot;
use tufo_models::library::{CollectionEntryId, CollectionId, CollectionKind};
use unicode_normalization::{UnicodeNormalization as _, char::is_combining_mark};

use crate::i18n::tr;

const RESULTS_MAX: usize = 25;
/// Meilisearch's cap; it bounds the work a pasted paragraph can cause.
const QUERY_WORDS_MAX: usize = 10;
/// Query words this long tolerate one typo, and two from the second length on.
const ONE_TYPO_WORD_LENGTH_MIN: usize = 5;
const TWO_TYPOS_WORD_LENGTH_MIN: usize = 9;
/// Two matched words farther apart than this count as unrelated.
const PROXIMITY_MAX: u32 = 7;

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
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) target: PaletteTarget,
}

/// What the palette looks for: words anywhere, or entry words within the collections named before
/// a slash, as in `boss/dragon`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaletteQuery {
    Everything(Vec<Word>),
    /// An empty `entry` lists every entry of the matching collections.
    Within {
        collection: Vec<Word>,
        entry: Vec<Word>,
    },
}

impl PaletteQuery {
    /// The query in `text`, or `None` when it has no words to look for.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let query_words = |text: &str| {
            let mut query_words = words(text);
            query_words.truncate(QUERY_WORDS_MAX);
            query_words
        };
        let query = match text.split_once('/') {
            Some((collection, entry)) => {
                let collection = query_words(collection);
                let entry = query_words(entry);
                if collection.is_empty() {
                    Self::Everything(entry)
                } else {
                    Self::Within { collection, entry }
                }
            }
            None => Self::Everything(query_words(text)),
        };
        match &query {
            Self::Everything(words) if words.is_empty() => None,
            _ => Some(query),
        }
    }
}

/// A word as search compares it: lowercase and without diacritics.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Word(Box<[char]>);

/// A word's place in the index's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WordId(usize);

/// The library's titles, split into words once per library change.
pub(crate) struct SearchIndex {
    vocabulary: Vocabulary,
    collections: Vec<IndexedCollection>,
}

/// Each distinct word once, so a query word is compared with each only once.
#[derive(Default)]
struct Vocabulary {
    words: Vec<Word>,
    ids: HashMap<Word, WordId>,
}

impl Vocabulary {
    fn title_words(&mut self, title: &str) -> Vec<WordId> {
        words(title).into_iter().map(|word| self.id(word)).collect()
    }

    fn id(&mut self, word: Word) -> WordId {
        if let Some(id) = self.ids.get(&word) {
            return *id;
        }
        let id = WordId(self.words.len());
        self.words.push(word.clone());
        self.ids.insert(word, id);
        id
    }
}

struct IndexedCollection {
    id: CollectionId,
    kind: CollectionKind,
    title: String,
    words: Vec<WordId>,
    /// Only the entries whose files exist.
    entries: Vec<IndexedEntry>,
}

struct IndexedEntry {
    id: CollectionEntryId,
    title: String,
    words: Vec<WordId>,
}

impl SearchIndex {
    pub(crate) fn new(snapshot: &LibrarySnapshot) -> Self {
        let mut vocabulary = Vocabulary::default();
        let collections = snapshot
            .collections
            .iter()
            .map(|collection| IndexedCollection {
                id: collection.id,
                kind: collection.kind,
                title: collection.title.as_str().to_owned(),
                words: vocabulary.title_words(collection.title.as_str()),
                entries: collection
                    .entries
                    .iter()
                    .filter(|entry| !entry.missing)
                    .map(|entry| IndexedEntry {
                        id: entry.id,
                        title: entry.title.as_str().to_owned(),
                        words: vocabulary.title_words(entry.title.as_str()),
                    })
                    .collect(),
            })
            .collect();
        Self {
            vocabulary,
            collections,
        }
    }

    /// Results for `query`, best first. An entry in two collections appears once for each.
    pub(crate) fn search(&self, query: &PaletteQuery) -> Vec<PaletteResult> {
        let mut ranked = match query {
            PaletteQuery::Everything(words) => self.rank_everything(&self.match_words(words)),
            PaletteQuery::Within { collection, entry } => self
                .rank_within_collections(&self.match_words(collection), &self.match_words(entry)),
        };
        let order = |left: &Ranked<'_>, right: &Ranked<'_>| {
            left.rank
                .cmp(&right.rank)
                .then_with(|| left.title.cmp(right.title))
                .then_with(|| left.subtitle.cmp(right.subtitle))
        };
        if ranked.len() > RESULTS_MAX {
            ranked.select_nth_unstable_by(RESULTS_MAX, order);
            ranked.truncate(RESULTS_MAX);
        }
        ranked.sort_unstable_by(order);
        ranked.into_iter().map(Ranked::into_result).collect()
    }

    fn match_words(&self, query: &[Word]) -> Vec<VocabularyMatches> {
        let mut distances = TypoDistances::default();
        query
            .iter()
            .map(|query_word| {
                VocabularyMatches(
                    self.vocabulary
                        .words
                        .iter()
                        .map(|word| distances.word_match(query_word, word))
                        .collect(),
                )
            })
            .collect()
    }

    fn rank_everything<'index>(&'index self, query: &[VocabularyMatches]) -> Vec<Ranked<'index>> {
        let mut ranked = Vec::new();
        for collection in &self.collections {
            ranked.extend(
                Rank::of(query, &collection.words, &[])
                    .map(|rank| Ranked::collection(rank, collection)),
            );
            ranked.extend(collection.entries.iter().filter_map(|entry| {
                Rank::of(query, &entry.words, &collection.words)
                    .map(|rank| Ranked::entry(rank, collection, entry))
            }));
        }
        ranked
    }

    fn rank_within_collections<'index>(
        &'index self,
        collection_query: &[VocabularyMatches],
        entry_query: &[VocabularyMatches],
    ) -> Vec<Ranked<'index>> {
        let mut ranked = Vec::new();
        for collection in &self.collections {
            let Some(collection_rank) = Rank::of(collection_query, &collection.words, &[]) else {
                continue;
            };
            ranked.extend(collection.entries.iter().filter_map(|entry| {
                let entry_rank = Rank::of(entry_query, &entry.words, &[])?;
                Some(Ranked::entry(
                    collection_rank.plus(entry_rank),
                    collection,
                    entry,
                ))
            }));
        }
        ranked
    }
}

/// How one query word matches each vocabulary word, by `WordId`.
struct VocabularyMatches(Vec<Option<WordMatch>>);

impl VocabularyMatches {
    /// The best match among `words`, with its position: the fewest typos, then a whole word over a
    /// beginning, then the earliest.
    fn best_in(&self, words: &[WordId]) -> Option<(u32, WordMatch)> {
        words
            .iter()
            .zip(0_u32..)
            .filter_map(|(id, position)| Some((position, self.0[id.0]?)))
            .min_by_key(|(position, word_match)| (*word_match, *position))
    }
}

/// How a query word matched a title word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct WordMatch {
    typos: u8,
    /// Only the title word's beginning matched, as while the query word is still being typed.
    beginning: bool,
}

/// Which title a query word matched in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchedTitle {
    Own,
    Collection,
}

#[derive(Debug, Clone, Copy)]
struct Hit {
    title: MatchedTitle,
    position: u32,
    word_match: WordMatch,
}

/// A result's place in the order, compared field by field; smaller ranks first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    typos: u32,
    words_in_collection_title: u32,
    proximity: u32,
    beginnings: u32,
    first_position: u32,
    title_words: u32,
}

impl Rank {
    /// The rank of a result titled `title`, in a collection titled `collection_title`, when every
    /// query word matches a word of either.
    fn of(
        query: &[VocabularyMatches],
        title: &[WordId],
        collection_title: &[WordId],
    ) -> Option<Self> {
        let mut rank = Self {
            title_words: u32::try_from(title.len()).unwrap_or(u32::MAX),
            ..Self::default()
        };
        let mut previous: Option<Hit> = None;
        let mut first_position = None;
        for query_word in query {
            let hit_in = |title: MatchedTitle, words: &[WordId]| {
                query_word.best_in(words).map(|(position, word_match)| Hit {
                    title,
                    position,
                    word_match,
                })
            };
            let hit = hit_in(MatchedTitle::Own, title)
                .or_else(|| hit_in(MatchedTitle::Collection, collection_title))?;
            rank.typos += u32::from(hit.word_match.typos);
            rank.beginnings += u32::from(hit.word_match.beginning);
            match hit.title {
                MatchedTitle::Own => {
                    first_position.get_or_insert(hit.position);
                }
                MatchedTitle::Collection => rank.words_in_collection_title += 1,
            }
            if let Some(previous) = previous {
                rank.proximity += proximity(previous, hit);
            }
            previous = Some(hit);
        }
        rank.first_position = first_position.unwrap_or(0);
        Some(rank)
    }

    fn plus(self, other: Self) -> Self {
        Self {
            typos: self.typos + other.typos,
            words_in_collection_title: self.words_in_collection_title
                + other.words_in_collection_title,
            proximity: self.proximity + other.proximity,
            beginnings: self.beginnings + other.beginnings,
            first_position: self.first_position + other.first_position,
            title_words: self.title_words + other.title_words,
        }
    }
}

/// How far apart two consecutive query words matched: nothing for neighbors in order, more for a
/// gap, and more again for the reverse order.
fn proximity(previous: Hit, next: Hit) -> u32 {
    if previous.title != next.title {
        return PROXIMITY_MAX;
    }
    let distance = if next.position > previous.position {
        next.position - previous.position - 1
    } else {
        previous.position - next.position + 1
    };
    distance.min(PROXIMITY_MAX)
}

struct Ranked<'index> {
    rank: Rank,
    title: &'index str,
    subtitle: &'index str,
    target: PaletteTarget,
}

impl<'index> Ranked<'index> {
    fn collection(rank: Rank, collection: &'index IndexedCollection) -> Self {
        Self {
            rank,
            title: &collection.title,
            subtitle: match collection.kind {
                CollectionKind::Playlist => tr("Playlist"),
                CollectionKind::Soundboard => tr("Soundboard"),
            },
            target: PaletteTarget::Collection {
                kind: collection.kind,
                id: collection.id,
            },
        }
    }

    fn entry(
        rank: Rank,
        collection: &'index IndexedCollection,
        entry: &'index IndexedEntry,
    ) -> Self {
        Self {
            rank,
            title: &entry.title,
            subtitle: &collection.title,
            target: match collection.kind {
                CollectionKind::Playlist => PaletteTarget::Track {
                    collection_id: collection.id,
                    entry_id: entry.id,
                },
                CollectionKind::Soundboard => PaletteTarget::Sound { entry_id: entry.id },
            },
        }
    }

    fn into_result(self) -> PaletteResult {
        PaletteResult {
            title: self.title.to_owned(),
            subtitle: self.subtitle.to_owned(),
            target: self.target,
        }
    }
}

/// Splits `text` into words: runs of letters or of digits, also split where a lowercase letter
/// meets an uppercase one, so `GoblinCave2` reads as `goblin cave 2`.
fn words(text: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut word = Vec::new();
    let mut previous: Option<char> = None;
    for character in text
        .nfd()
        .filter(|character| !is_combining_mark(*character))
    {
        if !character.is_alphanumeric() {
            finish_word(&mut words, &mut word);
            previous = None;
            continue;
        }
        if let Some(previous) = previous {
            let digits_meet_letters = previous.is_numeric() != character.is_numeric();
            let camel_case = previous.is_lowercase() && character.is_uppercase();
            if digits_meet_letters || camel_case {
                finish_word(&mut words, &mut word);
            }
        }
        word.extend(character.to_lowercase());
        previous = Some(character);
    }
    finish_word(&mut words, &mut word);
    words
}

fn finish_word(words: &mut Vec<Word>, word: &mut Vec<char>) {
    if !word.is_empty() {
        words.push(Word(std::mem::take(word).into_boxed_slice()));
    }
}

/// Damerau–Levenshtein distances in the optimal string alignment form, with rows kept between
/// calls.
#[derive(Default)]
struct TypoDistances {
    two_rows_up: Vec<usize>,
    row_up: Vec<usize>,
    row: Vec<usize>,
}

impl TypoDistances {
    /// How `query_word` matches `word`, if within the typos its length tolerates. As in
    /// Meilisearch, a typo in the first letter counts as two.
    fn word_match(&mut self, query_word: &Word, word: &Word) -> Option<WordMatch> {
        let (query_word, word) = (&*query_word.0, &*word.0);
        if word.starts_with(query_word) {
            return Some(WordMatch {
                typos: 0,
                beginning: word.len() > query_word.len(),
            });
        }
        let typos_max: usize = match query_word.len() {
            length if length >= TWO_TYPOS_WORD_LENGTH_MIN => 2,
            length if length >= ONE_TYPO_WORD_LENGTH_MIN => 1,
            _ => return None,
        };
        let first_letter_typo = usize::from(query_word.first() != word.first());
        // Neither distance can be zero here, so a remaining allowance of zero matches nothing.
        let distance_max = typos_max
            .checked_sub(first_letter_typo)
            .filter(|distance_max| *distance_max > 0)?;
        if word.len() + distance_max < query_word.len() {
            return None;
        }
        let (whole, beginning) = self.distances(query_word, word, distance_max)?;
        [(whole, false), (beginning, true)]
            .into_iter()
            .filter(|(distance, _)| *distance <= distance_max)
            .map(|(distance, beginning)| WordMatch {
                typos: u8::try_from(distance + first_letter_typo).unwrap_or(u8::MAX),
                beginning,
            })
            .min()
    }

    /// The distance from `query_word` to the whole `word`, and to the closest beginning of `word`,
    /// or `None` once both certainly exceed `distance_max`.
    fn distances(
        &mut self,
        query_word: &[char],
        word: &[char],
        distance_max: usize,
    ) -> Option<(usize, usize)> {
        // A beginning longer than this is farther than `distance_max`.
        let columns = word.len().min(query_word.len() + distance_max);
        let word_prefix = &word[..columns];
        self.two_rows_up.clear();
        self.row_up.clear();
        self.row_up.extend(0..=columns);
        for row_index in 0..query_word.len() {
            self.row.clear();
            self.row.push(row_index + 1);
            for column_index in 0..columns {
                let distance = self.cell(query_word, word_prefix, row_index, column_index);
                self.row.push(distance);
            }
            if self.row.iter().all(|distance| *distance > distance_max) {
                return None;
            }
            std::mem::swap(&mut self.two_rows_up, &mut self.row_up);
            std::mem::swap(&mut self.row_up, &mut self.row);
        }
        let beginning = self.row_up.iter().copied().min().unwrap_or(usize::MAX);
        let whole = if columns == word.len() {
            self.row_up[columns]
        } else {
            usize::MAX
        };
        Some((whole, beginning))
    }

    /// The distance between `query_word[..=row_index]` and `word[..=column_index]`, from the rows
    /// above and the cells before it in this row.
    fn cell(
        &self,
        query_word: &[char],
        word: &[char],
        row_index: usize,
        column_index: usize,
    ) -> usize {
        let (query_letter, word_letter) = (query_word[row_index], word[column_index]);
        let substitution = self.row_up[column_index] + usize::from(query_letter != word_letter);
        let deletion = self.row_up[column_index + 1] + 1;
        let insertion = self.row[column_index] + 1;
        let distance = substitution.min(deletion).min(insertion);
        let swapped = row_index > 0
            && column_index > 0
            && query_letter == word[column_index - 1]
            && query_word[row_index - 1] == word_letter;
        if swapped {
            distance.min(self.two_rows_up[column_index - 1] + 1)
        } else {
            distance
        }
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
            last_played_at: None,
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
            pinned: true,
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

    fn library_with(entries: &[&str]) -> LibrarySnapshot {
        let mut snapshot = library();
        snapshot.collections[0].entries.extend(
            entries
                .iter()
                .zip(100..)
                .map(|(title, seed)| entry(seed, title, false)),
        );
        snapshot
    }

    fn search(snapshot: &LibrarySnapshot, query: &str) -> Vec<PaletteResult> {
        PaletteQuery::parse(query)
            .map_or_else(Vec::new, |query| SearchIndex::new(snapshot).search(&query))
    }

    fn titles(snapshot: &LibrarySnapshot, query: &str) -> Vec<String> {
        search(snapshot, query)
            .into_iter()
            .map(|result| result.title)
            .collect()
    }

    #[test]
    fn the_best_match_wins_regardless_of_category() {
        let mut snapshot = library_with(&["Special Effects Ambience"]);
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
    fn a_word_in_the_title_outranks_one_in_its_collections_name() {
        let mut snapshot = library();
        snapshot.collections.push(collection(
            4,
            CollectionKind::Playlist,
            "Tavern",
            vec![entry(20, "Brawl at night", false)],
        ));

        assert_eq!(titles(&snapshot, "tavern brawl")[0], "Tavern Brawl");
    }

    #[test]
    fn a_slash_narrows_to_entries_of_a_named_collection() {
        assert_eq!(titles(&library(), "boss/brawl"), ["Tavern Brawl"]);
    }

    #[test]
    fn a_slash_with_nothing_after_it_lists_every_entry_of_the_collection() {
        let mut titles = titles(&library(), "boss/");
        titles.sort();

        assert_eq!(titles, ["Ancient Dragon", "Tavern Brawl"]);
    }

    #[test]
    fn nothing_before_the_slash_searches_everything_after_it() {
        let titles = titles(&library(), "/dragon");

        assert!(titles.contains(&"Ancient Dragon".to_owned()), "{titles:?}");
    }

    #[test]
    fn letters_scattered_across_a_title_do_not_match_a_word() {
        let snapshot = library_with(&["[dark]-ruin-gate dungeons", "[epic]-frost-titan-valgrim1"]);

        assert!(
            titles(&snapshot, "dragon")
                .iter()
                .all(|title| title.contains("Dragon"))
        );
        assert_eq!(titles(&snapshot, "heroes"), Vec::<String>::new());
    }

    #[test]
    fn words_split_at_punctuation_digits_and_inner_capitals() {
        let snapshot = library_with(&["[epic]-frost-titan-valgrim2", "GoblinCave"]);

        assert_eq!(
            titles(&snapshot, "valgrim 2"),
            ["[epic]-frost-titan-valgrim2"]
        );
        assert_eq!(titles(&snapshot, "cave"), ["GoblinCave"]);
    }

    #[test]
    fn a_query_word_matches_the_beginning_of_a_title_word() {
        assert_eq!(titles(&library(), "anc drag"), ["Ancient Dragon"]);
    }

    #[test]
    fn longer_query_words_tolerate_typos() {
        assert_eq!(titles(&library(), "tavren"), ["Tavern Brawl"]);
        assert_eq!(titles(&library(), "brwl"), Vec::<String>::new());
    }

    #[test]
    fn a_typo_in_the_first_letter_counts_twice() {
        assert_eq!(titles(&library(), "bavern"), Vec::<String>::new());
    }

    #[test]
    fn diacritics_and_case_do_not_matter() {
        let snapshot = library_with(&["Canção da Taverna"]);

        assert_eq!(titles(&snapshot, "CANCAO"), ["Canção da Taverna"]);
    }

    #[test]
    fn exact_typo_free_and_neighboring_words_rank_first() {
        let snapshot = library_with(&["Roar of the Dragon", "Dragons Roaring"]);

        assert_eq!(
            titles(&snapshot, "dragon roar"),
            ["Dragon roar", "Dragons Roaring", "Roar of the Dragon"]
        );
    }

    #[test]
    fn missing_entries_are_left_out() {
        assert!(!titles(&library(), "missing").contains(&"Dragon (missing)".to_owned()));
    }

    #[test]
    fn a_query_without_words_finds_nothing() {
        assert_eq!(PaletteQuery::parse("  "), None);
        assert_eq!(PaletteQuery::parse(" - / "), None);
    }
}
