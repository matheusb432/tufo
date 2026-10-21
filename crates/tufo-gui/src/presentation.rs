//! Turns the library snapshot, the player state, and the open page into what the window shows.
//!
//! Everything here is plain data, so it's computed off the UI thread and tested without a window.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    time::{Duration, SystemTime},
};

use tufo_client::{
    ClientError, Code,
    library::{
        Background, CollectionSource, CollectionView, EntryView, KenkuImportReport,
        LibraryFolderView, LibrarySnapshot, SongSource, SongView,
    },
    song_loops::{SavedSongLoop, SongLoopImportOutcome},
};
use tufo_models::{
    discord::{VoiceChannel, VoiceChannelTarget},
    library::{
        AudioFileId, BackgroundPreset, BuiltinTitle, CollectionEntryId, CollectionId,
        CollectionKind, EntryOrder, EntryOrderKey, LibraryFolderId, SoundIcon, Title,
    },
    path::ImageFilePath,
    playback::{
        OutputTarget,
        local_output::{LocalOutputFailureReason, LocalOutputState},
        player_state::{ActiveSound, MusicState, PlayerState, VoiceConnectionState},
        queue::{QueueModes, RepeatMode},
    },
};

use crate::i18n::{tr, tr_args, tr_count};

mod browser;

pub(crate) use browser::{BrowserPageView, BrowserStateView, browser_page};

/// How long the server may still list a sound after it ended; its chip stays full meanwhile.
const SOUND_END_GRACE: Duration = Duration::from_millis(250);
/// The Songs page lists at most this many rows; the filter finds the rest.
pub(crate) const SONG_ROWS_MAX: usize = 200;
/// A playlist's song search shows at most this many songs.
pub(crate) const SONG_SEARCH_RESULTS_MAX: usize = 20;

/// Which page is open, mirroring the window's `Page`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PageKind {
    #[default]
    Home,
    Songs,
    Playlists,
    Playlist,
    Soundboards,
    Soundboard,
    Browser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Navigation {
    pub(crate) page: PageKind,
    pub(crate) collection_id: Option<CollectionId>,
}

/// An sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Rgb(pub(crate) u8, pub(crate) u8, pub(crate) u8);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CardView {
    pub(crate) id: CollectionId,
    pub(crate) title: String,
    /// How many tracks or sounds it holds, such as "10 tracks".
    pub(crate) count_label: String,
    pub(crate) background: Background,
    pub(crate) playing: bool,
    pub(crate) missing: bool,
    pub(crate) pinned: bool,
}

/// A pinned collection in the sidebar's lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NavItemView {
    pub(crate) id: CollectionId,
    pub(crate) title: String,
    /// For the row's thumbnail.
    pub(crate) background: Background,
    pub(crate) playing: bool,
    pub(crate) missing: bool,
}

/// What the library pages ask of the snapshot besides the open page.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PageQueries<'a> {
    /// The open page's Ctrl+F filter.
    pub(crate) filter: &'a str,
    /// The search for songs to add to the open playlist.
    pub(crate) song_search: &'a str,
    /// The song the Add to playlist dialog adds.
    pub(crate) adding_song: Option<AudioFileId>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrackRowView {
    pub(crate) id: CollectionEntryId,
    pub(crate) song_id: AudioFileId,
    pub(crate) title: String,
    pub(crate) duration: String,
    pub(crate) date_added: String,
    pub(crate) source: String,
    pub(crate) missing: bool,
    pub(crate) current: bool,
    pub(crate) playing: bool,
    /// Its place in the queue, from 1.
    pub(crate) queue_number: Option<usize>,
    pub(crate) loop_badge: LoopBadgeView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopBadgeView {
    None,
    Saved,
    NeedsReview,
}

impl LoopBadgeView {
    fn of(saved_loop: Option<SavedSongLoop>) -> Self {
        match saved_loop {
            None => Self::None,
            Some(saved) if saved.needs_review => Self::NeedsReview,
            Some(_) => Self::Saved,
        }
    }
}

/// A song on the Songs page or among a playlist's song search results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SongRowView {
    pub(crate) id: AudioFileId,
    pub(crate) title: String,
    pub(crate) duration: String,
    pub(crate) date_added: String,
    pub(crate) source: String,
    pub(crate) missing: bool,
    /// Added by its own path, so it can leave the songs; a followed folder keeps the others.
    pub(crate) removable: bool,
    pub(crate) loop_badge: LoopBadgeView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SongLoopImportView {
    pub(crate) busy: bool,
    pub(crate) summary: String,
    pub(crate) rows: Vec<SongLoopImportRowView>,
    pub(crate) conflicts: usize,
    pub(crate) replace: bool,
    pub(crate) can_import: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SongLoopImportRowView {
    pub(crate) path: String,
    pub(crate) outcome: String,
    pub(crate) state: SongLoopImportRowState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SongLoopImportRowState {
    Ok,
    Conflict,
    Failed,
}

impl SongLoopImportRowState {
    pub(crate) fn of(outcome: SongLoopImportOutcome) -> Self {
        if outcome.is_failure() {
            Self::Failed
        } else if outcome == SongLoopImportOutcome::Conflict {
            Self::Conflict
        } else {
            Self::Ok
        }
    }
}

/// A followed music folder on the Songs page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SongFolderView {
    pub(crate) id: LibraryFolderId,
    pub(crate) path: String,
    /// How many songs it holds, such as "12 songs".
    pub(crate) count_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SongsPageView {
    /// How many songs there are, such as "120 songs".
    pub(crate) summary: String,
    pub(crate) folders: Vec<SongFolderView>,
    /// At most [`SONG_ROWS_MAX`] of the songs the filter matches.
    pub(crate) rows: Vec<SongRowView>,
    /// Says how many matching songs the rows leave out; empty when none.
    pub(crate) overflow: String,
}

/// A hand-picked playlist the Add to playlist dialog offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistChoiceView {
    pub(crate) id: CollectionId,
    pub(crate) title: String,
    pub(crate) count_label: String,
    /// It lists the song being added already.
    pub(crate) holds_song: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SoundTileView {
    pub(crate) id: CollectionEntryId,
    pub(crate) title: String,
    pub(crate) icon: SoundIcon,
    pub(crate) looping: bool,
    pub(crate) volume: f32,
    pub(crate) fade_in_ms: u32,
    pub(crate) fade_out_ms: u32,
    pub(crate) playing: bool,
    /// How far a playing sound is through its current pass, from 0 to 1.
    pub(crate) progress: f32,
    pub(crate) missing: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CollectionPageView {
    pub(crate) title: String,
    /// Its size, such as "10 tracks, 36 minutes" or "14 sounds, 4 playing".
    pub(crate) summary: String,
    pub(crate) background: Background,
    pub(crate) linked_folder: Option<String>,
    pub(crate) missing: bool,
    pub(crate) pinned: bool,
    pub(crate) order_key: EntryOrderKey,
    pub(crate) order_descending: bool,
    pub(crate) tracks: Vec<TrackRowView>,
    pub(crate) sounds: Vec<SoundTileView>,
    /// The songs a hand-picked playlist's song search finds that it doesn't list yet.
    pub(crate) song_search_results: Vec<SongRowView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputRowKind {
    Local,
    Guild,
    Channel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutputRowView {
    pub(crate) kind: OutputRowKind,
    pub(crate) label: String,
    pub(crate) initial: String,
    pub(crate) selected: bool,
    /// The row's output in the matching targets list, absent for guild headers.
    pub(crate) index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CurrentOutputView {
    pub(crate) kind: OutputRowKind,
    pub(crate) label: String,
    pub(crate) guild_name: String,
    pub(crate) guild_initial: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VoiceBadge {
    None,
    Joining,
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DrawerView {
    pub(crate) rows: Vec<OutputRowView>,
    pub(crate) current_output: CurrentOutputView,
    pub(crate) voice_status: String,
    pub(crate) voice_badge: VoiceBadge,
    pub(crate) discord_configured: bool,
    pub(crate) discord_disabled: bool,
}

/// The library pages' content for the open page and filter.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct LibraryPageView {
    pub(crate) playlists: Vec<CardView>,
    pub(crate) soundboards: Vec<CardView>,
    pub(crate) playlist_count: usize,
    pub(crate) soundboard_count: usize,
    /// The pinned collections of each kind in library order, whatever the filter, for the
    /// sidebar.
    pub(crate) nav_playlists: Vec<NavItemView>,
    pub(crate) nav_soundboards: Vec<NavItemView>,
    pub(crate) collection: Option<CollectionPageView>,
    pub(crate) songs: Option<SongsPageView>,
    /// Empty unless the Add to playlist dialog adds a song.
    pub(crate) playlist_choices: Vec<PlaylistChoiceView>,
}

pub(crate) fn library_page(
    snapshot: &LibrarySnapshot,
    player: &PlayerState,
    navigation: Navigation,
    queries: PageQueries<'_>,
    now: SystemTime,
) -> LibraryPageView {
    let card_filter = match navigation.page {
        PageKind::Home | PageKind::Playlists | PageKind::Soundboards => queries.filter,
        PageKind::Songs | PageKind::Playlist | PageKind::Soundboard | PageKind::Browser => "",
    };
    let songs = SongIndex::new(snapshot);
    let cards = |kind| {
        snapshot
            .collections
            .iter()
            .filter(|collection| collection.kind == kind)
            .filter(|collection| matches_filter(collection.title.as_str(), card_filter))
            .map(|collection| card(collection, player))
            .collect::<Vec<_>>()
    };
    let count = |kind| {
        snapshot
            .collections
            .iter()
            .filter(|collection| collection.kind == kind)
            .count()
    };
    let nav_items = |kind| {
        snapshot
            .collections
            .iter()
            .filter(|collection| collection.kind == kind && collection.pinned)
            .map(|collection| NavItemView {
                id: collection.id,
                title: collection.title.as_str().to_owned(),
                background: collection.background.clone(),
                playing: collection_plays(collection, player),
                missing: linked_folder_missing(collection),
            })
            .collect::<Vec<_>>()
    };
    let collection = match navigation.page {
        PageKind::Playlist | PageKind::Soundboard => navigation
            .collection_id
            .and_then(|id| find_collection(snapshot, id))
            .map(|collection| collection_page(collection, player, &songs, queries, now)),
        PageKind::Home
        | PageKind::Songs
        | PageKind::Playlists
        | PageKind::Soundboards
        | PageKind::Browser => None,
    };
    LibraryPageView {
        playlists: cards(CollectionKind::Playlist),
        soundboards: cards(CollectionKind::Soundboard),
        playlist_count: count(CollectionKind::Playlist),
        soundboard_count: count(CollectionKind::Soundboard),
        nav_playlists: nav_items(CollectionKind::Playlist),
        nav_soundboards: nav_items(CollectionKind::Soundboard),
        collection,
        songs: (navigation.page == PageKind::Songs).then(|| songs_page(&songs, queries.filter)),
        playlist_choices: queries
            .adding_song
            .map(|song_id| playlist_choices(snapshot, song_id))
            .unwrap_or_default(),
    }
}

/// The snapshot's songs by id, with the folders their sources name.
struct SongIndex<'a> {
    songs: &'a [SongView],
    by_id: HashMap<AudioFileId, &'a SongView>,
    folders: HashMap<LibraryFolderId, &'a LibraryFolderView>,
}

impl<'a> SongIndex<'a> {
    fn new(snapshot: &'a LibrarySnapshot) -> Self {
        Self {
            songs: &snapshot.songs,
            by_id: snapshot
                .songs
                .iter()
                .map(|song| (song.audio_file_id, song))
                .collect(),
            folders: snapshot
                .library_folders
                .iter()
                .map(|folder| (folder.id, folder))
                .collect(),
        }
    }

    /// Where the song comes from: the followed folder, or the file itself; empty when no
    /// source keeps it.
    fn source_label(&self, id: AudioFileId) -> String {
        let Some(song) = self.by_id.get(&id) else {
            return String::new();
        };
        let path = match song.source {
            SongSource::Folder(folder_id) => match self.folders.get(&folder_id) {
                Some(folder) => folder.folder.as_ref().as_path(),
                None => return String::new(),
            },
            SongSource::File => song.path.as_ref().as_path(),
        };
        path.to_string_lossy().into_owned()
    }

    fn row(&self, song: &SongView) -> SongRowView {
        SongRowView {
            id: song.audio_file_id,
            title: song.title.as_str().to_owned(),
            duration: clock(song.duration),
            date_added: date_added(song.added_at, jiff::tz::TimeZone::system()),
            source: self.source_label(song.audio_file_id),
            missing: song.missing,
            removable: song.source == SongSource::File,
            loop_badge: LoopBadgeView::of(song.saved_loop),
        }
    }

    /// The songs whose title or path holds every word of `query`.
    fn matching<'q>(&'q self, query: &'q str) -> impl Iterator<Item = &'a SongView> + 'q {
        self.songs.iter().filter(move |song| {
            query.trim().is_empty()
                || matches_filter(
                    &format!("{} {}", song.title, song.path.as_ref().display()),
                    query,
                )
        })
    }
}

fn songs_page(songs: &SongIndex<'_>, filter: &str) -> SongsPageView {
    let mut folder_song_counts = HashMap::<LibraryFolderId, usize>::new();
    for song in songs.songs {
        if let SongSource::Folder(id) = song.source {
            *folder_song_counts.entry(id).or_default() += 1;
        }
    }
    let mut folders = songs
        .folders
        .values()
        .filter(|folder| folder.kind == CollectionKind::Playlist)
        .map(|folder| SongFolderView {
            id: folder.id,
            path: folder.folder.as_ref().to_string_lossy().into_owned(),
            count_label: song_count_label(folder_song_counts.get(&folder.id).copied().unwrap_or(0)),
        })
        .collect::<Vec<_>>();
    folders.sort_by(|left, right| left.path.cmp(&right.path));
    let matching = songs.matching(filter).collect::<Vec<_>>();
    let overflow = matching.len().saturating_sub(SONG_ROWS_MAX);
    SongsPageView {
        summary: song_count_label(songs.songs.len()),
        folders,
        rows: matching
            .iter()
            .take(SONG_ROWS_MAX)
            .map(|song| songs.row(song))
            .collect(),
        overflow: if overflow == 0 {
            String::new()
        } else {
            tr_args(
                "Showing {0} of {1}. Filter to find the rest.",
                &[&SONG_ROWS_MAX, &song_count_label(matching.len())],
            )
        },
    }
}

fn song_count_label(count: usize) -> String {
    tr_count("{n} song", "{n} songs", count)
}

/// The hand-picked playlists, each saying whether it lists `song_id` already.
fn playlist_choices(snapshot: &LibrarySnapshot, song_id: AudioFileId) -> Vec<PlaylistChoiceView> {
    snapshot
        .collections
        .iter()
        .filter(|collection| {
            collection.kind == CollectionKind::Playlist
                && collection.source == CollectionSource::Manual
        })
        .map(|collection| PlaylistChoiceView {
            id: collection.id,
            title: collection.title.as_str().to_owned(),
            count_label: entry_count_label(collection.kind, collection.entries.len()),
            holds_song: collection
                .entries
                .iter()
                .any(|entry| entry.audio_file_id == song_id),
        })
        .collect()
}

/// The playable songs matching `query` that the playlist doesn't list.
fn song_search_results(
    collection: &CollectionView,
    songs: &SongIndex<'_>,
    query: &str,
) -> Vec<SongRowView> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let listed = collection
        .entries
        .iter()
        .map(|entry| entry.audio_file_id)
        .collect::<HashSet<_>>();
    songs
        .matching(query)
        .filter(|song| !song.missing && !listed.contains(&song.audio_file_id))
        .take(SONG_SEARCH_RESULTS_MAX)
        .map(|song| songs.row(song))
        .collect()
}

/// Moves an entry to `place` in the order the page shows and switches the collection to that
/// custom order. Returns whether the order changed.
pub(crate) fn arrange(
    collection: &mut CollectionView,
    entry_id: CollectionEntryId,
    place: usize,
) -> bool {
    let Some(from) = collection
        .entries
        .iter()
        .position(|entry| entry.id == entry_id)
    else {
        return false;
    };
    let place = place.min(collection.entries.len() - 1);
    if place == from {
        return false;
    }
    let entry = collection.entries.remove(from);
    collection.entries.insert(place, entry);
    collection.order = EntryOrder {
        key: EntryOrderKey::Custom,
        descending: false,
    };
    true
}

pub(crate) fn find_collection(
    snapshot: &LibrarySnapshot,
    id: CollectionId,
) -> Option<&CollectionView> {
    snapshot
        .collections
        .iter()
        .find(|collection| collection.id == id)
}

pub(crate) fn find_entry(
    snapshot: &LibrarySnapshot,
    id: CollectionEntryId,
) -> Option<(&CollectionView, &EntryView)> {
    snapshot.collections.iter().find_map(|collection| {
        collection
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| (collection, entry))
    })
}

/// Whether every whitespace-separated word of `filter` appears in `text`, ignoring case.
pub(crate) fn matches_filter(text: &str, filter: &str) -> bool {
    let text = text.to_lowercase();
    filter
        .split_whitespace()
        .all(|word| text.contains(&word.to_lowercase()))
}

/// A playlist whose music plays now, unpaused, or a soundboard with a sound playing.
fn collection_plays(collection: &CollectionView, player: &PlayerState) -> bool {
    match collection.kind {
        CollectionKind::Playlist => player
            .music_tracks()
            .any(|music| music.collection_id == collection.id && !music.paused),
        CollectionKind::Soundboard => collection
            .entries
            .iter()
            .any(|entry| player.sounds.iter().any(|sound| sound.entry_id == entry.id)),
    }
}

fn linked_folder_missing(collection: &CollectionView) -> bool {
    matches!(
        collection.source,
        CollectionSource::Linked { missing: true, .. }
    )
}

fn entry_count_label(kind: CollectionKind, count: usize) -> String {
    match kind {
        CollectionKind::Playlist => tr_count("{n} track", "{n} tracks", count),
        CollectionKind::Soundboard => tr_count("{n} sound", "{n} sounds", count),
    }
}

fn card(collection: &CollectionView, player: &PlayerState) -> CardView {
    CardView {
        id: collection.id,
        title: collection.title.as_str().to_owned(),
        count_label: entry_count_label(collection.kind, collection.entries.len()),
        background: collection.background.clone(),
        playing: collection_plays(collection, player),
        missing: linked_folder_missing(collection),
        pinned: collection.pinned,
    }
}

/// A collection's size: a playlist's tracks and running time in minutes, rounded up, or a
/// soundboard's sounds and how many play.
fn collection_summary(collection: &CollectionView, player: &PlayerState) -> String {
    let entries = entry_count_label(collection.kind, collection.entries.len());
    let detail = match collection.kind {
        CollectionKind::Playlist => {
            let total = collection
                .entries
                .iter()
                .map(|entry| entry.duration)
                .sum::<Duration>();
            let minutes =
                usize::try_from(total.as_secs().div_ceil(SECONDS_PER_MINUTE)).unwrap_or(usize::MAX);
            (minutes > 0).then(|| tr_count("{n} minute", "{n} minutes", minutes))
        }
        CollectionKind::Soundboard => {
            let playing = collection
                .entries
                .iter()
                .filter(|entry| player.sounds.iter().any(|sound| sound.entry_id == entry.id))
                .count();
            (playing > 0).then(|| tr_count("{n} playing", "{n} playing", playing))
        }
    };
    match detail {
        Some(detail) => tr_args("{0}, {1}", &[&entries, &detail]),
        None => entries,
    }
}

const SECONDS_PER_MINUTE: u64 = 60;

fn collection_page(
    collection: &CollectionView,
    player: &PlayerState,
    songs: &SongIndex<'_>,
    queries: PageQueries<'_>,
    now: SystemTime,
) -> CollectionPageView {
    let entries = collection
        .entries
        .iter()
        .filter(|entry| matches_filter(entry.title.as_str(), queries.filter));
    let (tracks, sounds) = match collection.kind {
        CollectionKind::Playlist => (
            entries
                .map(|entry| track_row(entry, player, songs))
                .collect(),
            Vec::new(),
        ),
        CollectionKind::Soundboard => (
            Vec::new(),
            entries
                .map(|entry| sound_tile(entry, player, now))
                .collect(),
        ),
    };
    let (linked_folder, missing) = match &collection.source {
        CollectionSource::Manual => (None, false),
        CollectionSource::Linked { folder, missing } => (
            Some(folder.as_ref().to_string_lossy().into_owned()),
            *missing,
        ),
    };
    CollectionPageView {
        title: collection.title.as_str().to_owned(),
        summary: collection_summary(collection, player),
        background: collection.background.clone(),
        linked_folder,
        missing,
        pinned: collection.pinned,
        order_key: collection.order.key,
        order_descending: collection.order.descending,
        tracks,
        sounds,
        song_search_results: match (collection.kind, &collection.source) {
            (CollectionKind::Playlist, CollectionSource::Manual) => {
                song_search_results(collection, songs, queries.song_search)
            }
            _ => Vec::new(),
        },
    }
}

fn track_row(entry: &EntryView, player: &PlayerState, songs: &SongIndex<'_>) -> TrackRowView {
    let current = player
        .music_tracks()
        .find(|music| music.entry.entry_id == entry.id);
    TrackRowView {
        id: entry.id,
        song_id: entry.audio_file_id,
        title: entry.title.as_str().to_owned(),
        duration: clock(entry.duration),
        date_added: date_added(entry.added_at, jiff::tz::TimeZone::system()),
        source: songs.source_label(entry.audio_file_id),
        missing: entry.missing,
        current: current.is_some(),
        playing: current.is_some_and(|music| !music.paused),
        queue_number: player
            .queued_entry_ids
            .iter()
            .position(|id| *id == entry.id)
            .map(|index| index + 1),
        loop_badge: LoopBadgeView::of(entry.saved_loop),
    }
}

fn date_added(timestamp: jiff::Timestamp, timezone: jiff::tz::TimeZone) -> String {
    timestamp
        .to_zoned(timezone)
        .strftime("%b %-d, %Y")
        .to_string()
}

fn sound_tile(entry: &EntryView, player: &PlayerState, now: SystemTime) -> SoundTileView {
    let sound = player
        .sounds
        .iter()
        .find(|sound| sound.entry_id == entry.id);
    SoundTileView {
        id: entry.id,
        title: entry.title.as_str().to_owned(),
        icon: entry.icon,
        looping: entry.sound.looping,
        volume: entry.sound.volume.into_inner(),
        fade_in_ms: millis(entry.sound.fade_in.into_inner()),
        fade_out_ms: millis(entry.sound.fade_out.into_inner()),
        playing: sound.is_some(),
        progress: sound.map_or(0.0, |sound| displayed_sound_progress(player, sound, now)),
        missing: entry.missing,
    }
}

fn millis(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

/// How the window names a background: a preset's position in [`BackgroundPreset::ALL`] or -1,
/// and an image file's path or an empty string.
pub(crate) fn background_fields(background: &Background) -> (i32, String) {
    match background {
        Background::Gradient => (-1, String::new()),
        Background::Preset(preset) => (i32::try_from(preset.index()).unwrap_or(-1), String::new()),
        Background::Image(path) => (-1, path.as_ref().to_string_lossy().into_owned()),
    }
}

/// The background the window's fields name, or `None` for an unknown preset or an image path
/// that isn't absolute.
pub(crate) fn background_from_fields(preset: i32, image_path: &str) -> Option<Background> {
    if !image_path.is_empty() {
        return ImageFilePath::try_new(PathBuf::from(image_path))
            .ok()
            .map(Background::Image);
    }
    match usize::try_from(preset) {
        Ok(index) => BackgroundPreset::ALL
            .get(index)
            .copied()
            .map(Background::Preset),
        Err(_) => Some(Background::Gradient),
    }
}

/// The name the background picker shows for a preset.
pub(crate) fn preset_name(preset: BackgroundPreset) -> &'static str {
    match preset {
        BackgroundPreset::Autumn => tr("Autumn"),
        BackgroundPreset::Battle => tr("Battle"),
        BackgroundPreset::Castle => tr("Castle"),
        BackgroundPreset::Cathedral => tr("Cathedral"),
        BackgroundPreset::Coast => tr("Coast"),
        BackgroundPreset::Desert => tr("Desert"),
        BackgroundPreset::Dragon => tr("Dragon"),
        BackgroundPreset::Fire => tr("Fire"),
        BackgroundPreset::Forest => tr("Forest"),
        BackgroundPreset::Haunted => tr("Haunted"),
        BackgroundPreset::Mountain => tr("Mountain"),
        BackgroundPreset::Night => tr("Night"),
        BackgroundPreset::Sky => tr("Sky"),
        BackgroundPreset::Skulls => tr("Skulls"),
        BackgroundPreset::Storm => tr("Storm"),
        BackgroundPreset::Tavern => tr("Tavern"),
        BackgroundPreset::Town => tr("Town"),
        BackgroundPreset::Underworld => tr("Underworld"),
        BackgroundPreset::Volcano => tr("Volcano"),
        BackgroundPreset::Winter => tr("Winter"),
    }
}

/// Who painted a preset, and when, as its credit line reads.
pub(crate) fn preset_credit(preset: BackgroundPreset) -> &'static str {
    match preset {
        BackgroundPreset::Autumn => tr("Jasper Francis Cropsey, 1860"),
        BackgroundPreset::Battle => tr("Charles de Steuben, 1830s"),
        BackgroundPreset::Castle => tr("Frederik Sødring, 1838"),
        BackgroundPreset::Cathedral => tr("John Constable, about 1825"),
        BackgroundPreset::Coast => tr("William Stanley Haseltine, 1869"),
        BackgroundPreset::Desert => tr("Jean-Léon Gérôme, 1880s"),
        BackgroundPreset::Dragon => tr("Paolo Uccello, about 1470"),
        BackgroundPreset::Fire => tr("J. M. W. Turner, 1835"),
        BackgroundPreset::Forest => tr("Jacob van Ruisdael, about 1655"),
        BackgroundPreset::Haunted => tr("Arnold Böcklin, 1880"),
        BackgroundPreset::Mountain => tr("Albert Bierstadt, 1863"),
        BackgroundPreset::Night => tr("Claude-Joseph Vernet, 1772"),
        BackgroundPreset::Sky => tr("Frederic Edwin Church, 1860"),
        BackgroundPreset::Skulls => tr("Pieter Claesz, 1628"),
        BackgroundPreset::Storm => tr("Ludolf Bakhuysen, about 1695"),
        BackgroundPreset::Tavern => tr("David Teniers the Younger, 1658"),
        BackgroundPreset::Town => tr("Bernardo Bellotto, about 1761"),
        BackgroundPreset::Underworld => tr("John Martin, 1841"),
        BackgroundPreset::Volcano => tr("Pierre-Jacques Volaire, 1771"),
        BackgroundPreset::Winter => tr("Hendrick Avercamp, about 1608"),
    }
}

/// The background picker's groups of presets, by the kind of scene a game master looks for.
pub(crate) const PRESET_GROUPS: [(&str, &[BackgroundPreset]); 3] = [
    (
        "Places",
        &[
            BackgroundPreset::Tavern,
            BackgroundPreset::Town,
            BackgroundPreset::Castle,
            BackgroundPreset::Cathedral,
            BackgroundPreset::Forest,
            BackgroundPreset::Mountain,
            BackgroundPreset::Coast,
            BackgroundPreset::Desert,
        ],
    ),
    (
        "Perils",
        &[
            BackgroundPreset::Battle,
            BackgroundPreset::Dragon,
            BackgroundPreset::Fire,
            BackgroundPreset::Storm,
            BackgroundPreset::Volcano,
            BackgroundPreset::Haunted,
            BackgroundPreset::Skulls,
            BackgroundPreset::Underworld,
        ],
    ),
    (
        "Skies and seasons",
        &[
            BackgroundPreset::Sky,
            BackgroundPreset::Night,
            BackgroundPreset::Autumn,
            BackgroundPreset::Winter,
        ],
    ),
];

/// A background's name and the line under it in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackgroundLabel {
    pub(crate) name: String,
    pub(crate) credit: String,
}

pub(crate) fn background_label(background: &Background) -> BackgroundLabel {
    match background {
        Background::Gradient => BackgroundLabel {
            name: tr("Gradient").to_owned(),
            credit: tr("Colors drawn from the collection").to_owned(),
        },
        Background::Preset(preset) => BackgroundLabel {
            name: preset_name(*preset).to_owned(),
            credit: preset_credit(*preset).to_owned(),
        },
        Background::Image(path) => {
            let path = path.as_ref();
            BackgroundLabel {
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                credit: path
                    .parent()
                    .map(|folder| folder.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            }
        }
    }
}

/// The presets the library's collections show, for picking a new collection's preset.
pub(crate) fn shown_presets(snapshot: &LibrarySnapshot) -> Vec<BackgroundPreset> {
    snapshot
        .collections
        .iter()
        .filter_map(|collection| match collection.background {
            Background::Preset(preset) => Some(preset),
            Background::Gradient | Background::Image(_) => None,
        })
        .collect()
}

/// The hues cards draw from: yellow-green through teal, so every card sits in the green palette.
const CARD_HUE_DEGREES_MIN: u16 = 80;
const CARD_HUE_DEGREES_SPAN: u16 = 100;

/// A gradient derived from the collection's id, so each card keeps its colors.
pub(crate) fn gradient(id: CollectionId) -> (Rgb, Rgb) {
    let bytes = id.into_uuid().into_bytes();
    let hue = f32::from(
        CARD_HUE_DEGREES_MIN + u16::from_le_bytes([bytes[14], bytes[15]]) % CARD_HUE_DEGREES_SPAN,
    );
    (hsv(hue, 0.5, 0.55), hsv(hue + 20.0, 0.7, 0.22))
}

fn hsv(hue: f32, saturation: f32, value: f32) -> Rgb {
    let chroma = value * saturation;
    let sector = hue / 60.0;
    let second = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (red, green, blue) = match sector {
        sector if sector < 1.0 => (chroma, second, 0.0),
        sector if sector < 2.0 => (second, chroma, 0.0),
        sector if sector < 3.0 => (0.0, chroma, second),
        sector if sector < 4.0 => (0.0, second, chroma),
        sector if sector < 5.0 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    let lift = value - chroma;
    let channel = |component: f32| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the channel is clamped to 0..=255 first"
        )]
        let byte = ((component + lift) * 255.0).round().clamp(0.0, 255.0) as u8;
        byte
    };
    Rgb(channel(red), channel(green), channel(blue))
}

/// A duration as `m:ss`, or `h:mm:ss` from an hour on.
pub(crate) fn clock(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// The player bar's text and progress at `now`.
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "it mirrors the window's player properties one to one"
)]
pub(crate) struct PlayerBarView {
    pub(crate) local_output_message: String,
    pub(crate) local_output_recovering: bool,
    pub(crate) has_music: bool,
    pub(crate) has_playback: bool,
    pub(crate) entry_id: Option<CollectionEntryId>,
    pub(crate) can_add_layer: bool,
    pub(crate) layer_count: i32,
    pub(crate) music_layers: Vec<MusicLayerView>,
    pub(crate) title: String,
    /// The playing collection, whose background the player shows beside its title.
    pub(crate) collection_id: Option<CollectionId>,
    pub(crate) collection_title: String,
    pub(crate) collection_background: Background,
    pub(crate) playing: bool,
    pub(crate) progress: f32,
    pub(crate) elapsed: String,
    pub(crate) remaining: String,
    pub(crate) volume: f32,
    pub(crate) muted: bool,
    pub(crate) effects_volume: f32,
    pub(crate) shuffle: bool,
    pub(crate) repeat: i32,
    pub(crate) songs_queued: bool,
    pub(crate) sounds: Vec<SoundChipView>,
    pub(crate) sounds_paused: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MusicLayerView {
    pub(crate) id: CollectionEntryId,
    pub(crate) title: String,
    pub(crate) volume: f32,
    pub(crate) looping: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SoundChipView {
    pub(crate) id: CollectionEntryId,
    pub(crate) title: String,
    pub(crate) progress: f32,
    pub(crate) looping: bool,
}

pub(crate) fn player_bar(
    snapshot: &LibrarySnapshot,
    player: &PlayerState,
    now: SystemTime,
) -> PlayerBarView {
    let music = player.music.as_ref();
    let (elapsed, duration) = music.map_or((Duration::ZERO, None), |music| {
        let now = if local_output_interrupted(player) {
            music.position.sampled_at
        } else {
            now
        };
        (music_position(music, player.modes, now), music.duration)
    });
    let progress = duration
        .filter(|duration| !duration.is_zero())
        .map_or(0.0, |duration| {
            (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
        });
    let remaining = duration.map_or_else(
        || "-0:00".to_owned(),
        |duration| format!("-{}", clock(duration.saturating_sub(elapsed))),
    );
    let sounds = player
        .sounds
        .iter()
        .map(|sound| {
            let title = find_entry(snapshot, sound.entry_id).map_or_else(
                || tr("Sound").to_owned(),
                |(_, entry)| entry.title.as_str().to_owned(),
            );
            SoundChipView {
                id: sound.entry_id,
                title,
                progress: displayed_sound_progress(player, sound, now),
                looping: sound.looping,
            }
        })
        .collect();
    let displayed_music = player.music_tracks().next();
    let playing_collection =
        displayed_music.and_then(|music| find_collection(snapshot, music.collection_id));
    PlayerBarView {
        local_output_message: local_output_message(player),
        local_output_recovering: matches!(player.local_output, LocalOutputState::Recovering { .. }),
        has_music: music.is_some(),
        has_playback: displayed_music.is_some(),
        entry_id: music.map(|music| music.entry.entry_id),
        can_add_layer: music.is_none()
            || player.music_layers.len()
                < tufo_models::playback::music_layers::MUSIC_LAYERS_EXTRA_MAX,
        layer_count: i32::try_from(player.music_layers.len() + usize::from(music.is_some()))
            .unwrap_or(0),
        music_layers: player
            .music_layers
            .iter()
            .map(|(_, layer)| MusicLayerView {
                id: layer.music.entry.entry_id,
                title: find_entry(snapshot, layer.music.entry.entry_id).map_or_else(
                    || layer.music.entry.title.clone(),
                    |(_, entry)| entry.title.as_str().to_owned(),
                ),
                volume: layer.volume.into_inner(),
                looping: layer.looping,
            })
            .collect(),
        collection_id: displayed_music.map(|music| music.collection_id),
        collection_background: playing_collection.map_or(Background::Gradient, |collection| {
            collection.background.clone()
        }),
        title: displayed_music.map_or_else(String::new, |music| {
            find_entry(snapshot, music.entry.entry_id).map_or_else(
                || music.entry.title.clone(),
                |(_, entry)| entry.title.as_str().to_owned(),
            )
        }),
        collection_title: playing_collection.map_or_else(String::new, |collection| {
            collection.title.as_str().to_owned()
        }),
        playing: player.music_tracks().any(|music| !music.paused),
        progress,
        elapsed: clock(elapsed),
        remaining,
        volume: player.volume.into_inner(),
        effects_volume: player.effects_volume.into_inner(),
        muted: player.muted,
        shuffle: player.modes.shuffle,
        repeat: match player.modes.repeat {
            RepeatMode::Off => 0,
            RepeatMode::Playlist => 1,
            RepeatMode::Track => 2,
        },
        songs_queued: !player.queued_entry_ids.is_empty(),
        sounds,
        sounds_paused: player.sounds_paused,
    }
}

/// The repeat mode after `repeat`, the way Kenku FM cycles: playlist, then track, then off.
/// Queued songs skip repeating one track, which excludes them.
pub(crate) fn next_repeat_mode(repeat: RepeatMode, songs_queued: bool) -> RepeatMode {
    match repeat {
        RepeatMode::Playlist if !songs_queued => RepeatMode::Track,
        RepeatMode::Playlist | RepeatMode::Track => RepeatMode::Off,
        RepeatMode::Off => RepeatMode::Playlist,
    }
}

/// How far a sound shows through its pass: a paused sound's start moves only once it resumes, so
/// its progress shows as none meanwhile.
fn displayed_sound_progress(player: &PlayerState, sound: &ActiveSound, now: SystemTime) -> f32 {
    if player.sounds_paused || local_output_interrupted(player) {
        0.0
    } else {
        sound_progress(sound, now)
    }
}

fn local_output_interrupted(player: &PlayerState) -> bool {
    player.output == OutputTarget::Local
        && matches!(
            player.local_output,
            LocalOutputState::Failed(_) | LocalOutputState::Recovering { .. }
        )
}

fn local_output_message(player: &PlayerState) -> String {
    if player.output != OutputTarget::Local {
        return String::new();
    }
    let message = match &player.local_output {
        LocalOutputState::Released | LocalOutputState::Ready(_) => return String::new(),
        LocalOutputState::Recovering { .. } => {
            tr("Reconnecting local audio… Your playback settings are kept.")
        }
        LocalOutputState::Failed(failure) => match failure.reason {
            LocalOutputFailureReason::NoDevice | LocalOutputFailureReason::DeviceUnavailable => {
                tr("No audio device is available. Connect one, then retry.")
            }
            LocalOutputFailureReason::TargetMissing => {
                tr("The selected audio device is unavailable. Reconnect it, then retry.")
            }
            LocalOutputFailureReason::StreamInvalidated
            | LocalOutputFailureReason::DeviceChanged
            | LocalOutputFailureReason::CallbackStopped => {
                tr("Local audio stopped. Check your audio device, then retry.")
            }
            LocalOutputFailureReason::OpenFailed => {
                tr("Local audio could not start. Check your audio device, then retry.")
            }
            LocalOutputFailureReason::OpenTimedOut => {
                tr("The audio device did not respond. Check it, then retry.")
            }
        },
    };
    message.to_owned()
}

/// How far `sound` is through its current pass at `now`. A sound that stops looping finishes the
/// pass it's in, so every pass starts at a multiple of the duration after the sound started.
fn sound_progress(sound: &ActiveSound, now: SystemTime) -> f32 {
    if sound.duration.is_zero() {
        return 0.0;
    }
    let since_start = now.duration_since(sound.started_at).unwrap_or_default();
    let pass_position = since_start.as_secs_f32() % sound.duration.as_secs_f32();
    let ended = !sound.looping
        && since_start >= sound.duration
        && pass_position < SOUND_END_GRACE.as_secs_f32();
    if ended {
        1.0
    } else {
        pass_position / sound.duration.as_secs_f32()
    }
}

/// Wraps within a saved loop while the track repeats.
pub(crate) fn music_position(music: &MusicState, modes: QueueModes, now: SystemTime) -> Duration {
    let position = music.position_at(now, modes.repeat == RepeatMode::Track);
    music
        .duration
        .map_or(position, |duration| position.min(duration))
}

/// Where the track stops when it doesn't repeat: its loop end, or its file's end.
pub(crate) fn music_end(music: &MusicState) -> Option<Duration> {
    music
        .loop_region
        .map(|region| region.loop_end())
        .or(music.duration)
}

/// `targets[i]` is the output of the row whose `index` is `Some(i)`.
pub(crate) fn drawer(
    state: &PlayerState,
    voice_channels: &[VoiceChannel],
) -> (DrawerView, Vec<OutputTarget>) {
    let (rows, targets) = output_rows(state, voice_channels);
    let view = DrawerView {
        rows,
        current_output: current_output(state, voice_channels),
        voice_status: voice_status(&state.voice),
        voice_badge: voice_badge(state),
        discord_configured: !matches!(state.voice, VoiceConnectionState::NotConfigured),
        discord_disabled: matches!(state.voice, VoiceConnectionState::Disabled),
    };
    (view, targets)
}

/// Whether the bot can join a voice channel: Discord is configured and its connection enabled.
pub(crate) fn discord_connection_enabled(voice: &VoiceConnectionState) -> bool {
    !matches!(
        voice,
        VoiceConnectionState::NotConfigured | VoiceConnectionState::Disabled
    )
}

/// The drawer's output rows: this computer, then each guild's channels by position while the bot
/// can join them.
fn output_rows(
    state: &PlayerState,
    voice_channels: &[VoiceChannel],
) -> (Vec<OutputRowView>, Vec<OutputTarget>) {
    let voice_channels = if discord_connection_enabled(&state.voice) {
        voice_channels
    } else {
        &[]
    };
    let mut guilds = BTreeMap::<(&str, u64), Vec<&VoiceChannel>>::new();
    for channel in voice_channels {
        guilds
            .entry((
                channel.guild_name.as_str(),
                channel.target.guild_id.into_inner().get(),
            ))
            .or_default()
            .push(channel);
    }
    let mut rows = vec![OutputRowView {
        kind: OutputRowKind::Local,
        label: tr("This Computer").to_owned(),
        initial: String::new(),
        selected: state.output == OutputTarget::Local,
        index: Some(0),
    }];
    let mut targets = vec![OutputTarget::Local];
    for ((guild_name, _), mut channels) in guilds {
        channels.sort_by(|left, right| {
            (left.position, &left.channel_name).cmp(&(right.position, &right.channel_name))
        });
        rows.push(OutputRowView {
            kind: OutputRowKind::Guild,
            label: guild_name.to_owned(),
            initial: initial_of(guild_name),
            selected: false,
            index: None,
        });
        for channel in channels {
            let target = OutputTarget::DiscordVoice(channel.target);
            rows.push(OutputRowView {
                kind: OutputRowKind::Channel,
                label: channel.channel_name.clone(),
                initial: String::new(),
                selected: state.output == target,
                index: Some(targets.len()),
            });
            targets.push(target);
        }
    }
    // An active channel the list no longer shows still gets a row, so the drawer stays true.
    if !targets.contains(&state.output)
        && let OutputTarget::DiscordVoice(target) = state.output
    {
        rows.push(OutputRowView {
            kind: OutputRowKind::Channel,
            label: unlisted_channel_label(target),
            initial: String::new(),
            selected: true,
            index: Some(targets.len()),
        });
        targets.push(state.output);
    }
    (rows, targets)
}

fn initial_of(name: &str) -> String {
    name.chars()
        .next()
        .map(|letter| letter.to_uppercase().collect())
        .unwrap_or_default()
}

fn unlisted_channel_label(target: VoiceChannelTarget) -> String {
    tr_args("Discord channel {0}", &[&target.channel_id.into_inner()])
}

fn current_output(state: &PlayerState, voice_channels: &[VoiceChannel]) -> CurrentOutputView {
    let OutputTarget::DiscordVoice(target) = state.output else {
        return CurrentOutputView {
            kind: OutputRowKind::Local,
            label: tr("This Computer").to_owned(),
            guild_name: String::new(),
            guild_initial: String::new(),
        };
    };
    match voice_channels
        .iter()
        .find(|channel| channel.target == target)
    {
        Some(channel) => CurrentOutputView {
            kind: OutputRowKind::Channel,
            label: channel.channel_name.clone(),
            guild_name: channel.guild_name.clone(),
            guild_initial: initial_of(&channel.guild_name),
        },
        None => CurrentOutputView {
            kind: OutputRowKind::Channel,
            label: unlisted_channel_label(target),
            guild_name: String::new(),
            guild_initial: String::new(),
        },
    }
}

fn voice_badge(state: &PlayerState) -> VoiceBadge {
    match (state.output, &state.voice) {
        (OutputTarget::Local, _)
        | (
            _,
            VoiceConnectionState::NotConfigured
            | VoiceConnectionState::Connected
            | VoiceConnectionState::Disabled,
        ) => VoiceBadge::None,
        (_, VoiceConnectionState::Connecting | VoiceConnectionState::Reconnecting { .. }) => {
            VoiceBadge::Joining
        }
        (_, VoiceConnectionState::Disconnected { .. }) => VoiceBadge::Dropped,
    }
}

/// A line under the outputs about the bot's voice connection; empty when there's nothing to say.
fn voice_status(voice: &VoiceConnectionState) -> String {
    match voice {
        VoiceConnectionState::NotConfigured
        | VoiceConnectionState::Connected
        | VoiceConnectionState::Disabled
        | VoiceConnectionState::Disconnected { .. } => String::new(),
        VoiceConnectionState::Connecting => tr("Joining the voice channel…").to_owned(),
        VoiceConnectionState::Reconnecting { attempt } => {
            tr_args("Voice dropped; reconnecting (attempt {0})…", &[attempt])
        }
    }
}

pub(crate) fn localize_snapshot(snapshot: &mut LibrarySnapshot) {
    use tufo_models::library::compare_titles;

    for collection in &mut snapshot.collections {
        if let Some(key) = collection.builtin_title {
            collection.title = Title::from_name_lossy(builtin_title(key));
        }
        for entry in &mut collection.entries {
            if let Some(key) = entry.builtin_title {
                entry.title = Title::from_name_lossy(builtin_title(key));
            }
        }
        collection.entries.sort_by(|left, right| {
            let by_key = match collection.order.key {
                EntryOrderKey::Title => compare_titles(left.title.as_str(), right.title.as_str()),
                EntryOrderKey::Duration => left.duration.cmp(&right.duration),
                EntryOrderKey::DateAdded => left.added_at.cmp(&right.added_at),
                // The server sends the custom order, which a stable sort keeps.
                EntryOrderKey::Custom => return Ordering::Equal,
            };
            let by_key = if collection.order.descending {
                by_key.reverse()
            } else {
                by_key
            };
            by_key
                .then_with(|| compare_titles(left.title.as_str(), right.title.as_str()))
                .then_with(|| left.id.cmp(&right.id))
        });
    }
    snapshot.collections.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| compare_titles(left.title.as_str(), right.title.as_str()))
            .then_with(|| left.id.cmp(&right.id))
    });
    for song in &mut snapshot.songs {
        if let Some(key) = song.builtin_title {
            song.title = Title::from_name_lossy(builtin_title(key));
        }
    }
    snapshot.songs.sort_by(|left, right| {
        compare_titles(left.title.as_str(), right.title.as_str())
            .then_with(|| left.audio_file_id.cmp(&right.audio_file_id))
    });
}

fn builtin_title(key: BuiltinTitle) -> &'static str {
    match key {
        BuiltinTitle::Effects => tr("Effects"),
        BuiltinTitle::Imported => tr("Imported"),
        BuiltinTitle::Rain => tr("Rain"),
        BuiltinTitle::Storm => tr("Storm"),
        BuiltinTitle::Wind => tr("Wind"),
        BuiltinTitle::Waves => tr("Waves"),
        BuiltinTitle::Stream => tr("Stream"),
        BuiltinTitle::Birds => tr("Birds"),
        BuiltinTitle::SummerNight => tr("Summer Night"),
        BuiltinTitle::Fireplace => tr("Fireplace"),
    }
}

/// Server diagnostics never become GUI text, including unknown or old-server errors.
pub(crate) fn error_text(error: &ClientError) -> String {
    let ClientError::Status(status) = error else {
        return tr("The server sent an invalid response. Update the app and server together.")
            .to_owned();
    };
    if let Some(reason) = tufo_client::error::reason(status)
        && let Some(text) = reason_text(status.code(), &reason)
    {
        return text.to_owned();
    }
    match status.code() {
        Code::Unavailable => tr("The server is unavailable. Reconnecting shortly."),
        Code::DeadlineExceeded => tr("The request took too long. Try again."),
        Code::Cancelled => tr("The request was cancelled."),
        Code::PermissionDenied | Code::Unauthenticated => tr("Permission denied for this request."),
        Code::InvalidArgument | Code::OutOfRange => {
            tr("Some values are invalid. Check them and try again.")
        }
        Code::Unimplemented => tr("Update the app and server together to use this feature."),
        Code::ResourceExhausted => tr("The server is busy. Try again shortly."),
        _ => tr("Could not complete the request. Check the server log."),
    }
    .to_owned()
}

/// Reasons about the library: collections, entries, and Kenku FM imports.
fn library_reason_text(code: Code, reason: &str) -> Option<&'static str> {
    use tufo_application::{
        library::{
            LibraryStorageError,
            add_files_to_collection::AddFilesToCollectionError,
            add_songs_to_playlist::AddSongsToPlaylistError,
            arrange_collection_entries::ArrangeCollectionEntriesError,
            delete_collection::DeleteCollectionError,
            delete_collection_entry::DeleteCollectionEntryError,
            delete_library_folder::DeleteLibraryFolderError,
            delete_song::DeleteSongError,
            import_kenku_library::{ImportKenkuLibraryError, kenku_data::ReadKenkuDataError},
            update_collection::UpdateCollectionError,
            update_collection_entry::UpdateCollectionEntryError,
        },
        playback::{play_collection::PlayCollectionError, play_entry_sound::PlayEntrySoundError},
    };
    Some(match (code, reason) {
        (Code::InvalidArgument, ImportKenkuLibraryError::CHOICES) => tr(
            "Choose Keep or Overwrite for each collection. Overwrite each existing collection only once.",
        ),
        (Code::FailedPrecondition, ImportKenkuLibraryError::BUSY) => {
            tr("A Kenku FM import is already in progress.")
        }
        (Code::Internal, LibraryStorageError::REASON) => {
            tr("Could not save or read the library. Check the server log.")
        }
        (
            Code::NotFound,
            AddFilesToCollectionError::NOT_FOUND
            | AddSongsToPlaylistError::PLAYLIST_NOT_FOUND
            | DeleteCollectionError::NOT_FOUND
            | UpdateCollectionError::NOT_FOUND
            | ArrangeCollectionEntriesError::NOT_FOUND
            | PlayCollectionError::NOT_FOUND,
        ) => tr("That collection no longer exists."),
        (Code::NotFound, AddSongsToPlaylistError::SONG_NOT_FOUND | DeleteSongError::NOT_FOUND) => {
            tr("That song is no longer among your songs.")
        }
        (Code::FailedPrecondition, DeleteSongError::IN_FOLLOWED_FOLDER) => {
            tr("A followed folder holds this song. Stop following the folder to remove it.")
        }
        (Code::NotFound, DeleteLibraryFolderError::NOT_FOUND) => {
            tr("That folder is no longer followed.")
        }
        (
            Code::NotFound,
            DeleteCollectionEntryError::NOT_FOUND
            | UpdateCollectionEntryError::NOT_FOUND
            | ArrangeCollectionEntriesError::ENTRY_NOT_FOUND
            | PlayEntrySoundError::NOT_FOUND,
        ) => tr("That track or sound no longer exists."),
        (
            Code::FailedPrecondition,
            AddFilesToCollectionError::LINKED
            | AddSongsToPlaylistError::LINKED
            | DeleteCollectionEntryError::LINKED,
        ) => tr("This collection follows a folder. Change the files in that folder."),
        (Code::NotFound, ReadKenkuDataError::NOT_FOUND) => {
            tr("No Kenku FM library was found in that folder.")
        }
        (Code::FailedPrecondition, ReadKenkuDataError::UNREADABLE) => {
            tr("Could not read Kenku FM’s library. Check the folder permissions.")
        }
        (Code::FailedPrecondition, ReadKenkuDataError::RUNNING) => {
            tr("Close Kenku FM and try again.")
        }
        (Code::FailedPrecondition, ReadKenkuDataError::MALFORMED) => {
            tr("Kenku FM’s library is invalid. Quit Kenku FM and try again.")
        }
        (Code::FailedPrecondition, PlayCollectionError::NOTHING_PLAYABLE) => {
            tr("There are no playable files in this collection.")
        }
        (Code::FailedPrecondition, PlayCollectionError::START_NOT_PLAYABLE) => {
            tr("That track is missing or cannot be played.")
        }
        _ => return None,
    })
}

fn song_loop_reason_text(code: Code, reason: &str) -> Option<&'static str> {
    use tufo_application::{
        library::{
            get_song_waveform::GetSongWaveformError, import_song_loops::ImportSongLoopsError,
            song_analysis::AnalyzeSongError, update_song_loop::UpdateSongLoopError,
        },
        playback::preview_song_loop::PreviewSongLoopError,
    };
    Some(match (code, reason) {
        (
            Code::NotFound,
            GetSongWaveformError::NOT_FOUND
            | UpdateSongLoopError::NOT_FOUND
            | PreviewSongLoopError::NOT_FOUND,
        ) => tr("That song is no longer among your songs."),
        (Code::FailedPrecondition, AnalyzeSongError::MISSING) => {
            tr("The audio file is missing. Restore it and try again.")
        }
        (
            Code::FailedPrecondition,
            AnalyzeSongError::UNREADABLE | AnalyzeSongError::UNSUPPORTED,
        ) => tr("Could not read this song’s audio."),
        (
            Code::FailedPrecondition,
            UpdateSongLoopError::SOURCE_CHANGED | PreviewSongLoopError::SOURCE_CHANGED,
        ) => tr("The song’s file changed while the editor was open. Open the loop again."),
        (Code::InvalidArgument, UpdateSongLoopError::INVALID_MARKERS) => {
            tr("Place Start, Loop, and End in that order inside the song.")
        }
        (Code::InvalidArgument, ImportSongLoopsError::INVALID_DOCUMENT) => {
            tr("That file is not a song loop export.")
        }
        _ => return None,
    })
}

fn reason_text(code: Code, reason: &str) -> Option<&'static str> {
    use tufo_application::{
        discord::{
            bot_credentials::CredentialsError, discord_client::DiscordClientError,
            set_discord_bot_token::SetDiscordBotTokenError,
        },
        playback::{
            add_music_layer::AddMusicLayerError, add_to_queue::AddToQueueError,
            control_music::ControlMusicError, open_browser::OpenBrowserError,
            play_entry_sound::PlayEntrySoundError, player::PlayAudioError,
            select_output::SelectOutputError,
        },
    };
    if let Some(text) =
        library_reason_text(code, reason).or_else(|| song_loop_reason_text(code, reason))
    {
        return Some(text);
    }
    Some(match (code, reason) {
        (Code::Unavailable, OpenBrowserError::UNAVAILABLE) => {
            tr("This installation has no browser. It runs on Linux and Windows.")
        }
        (Code::Unavailable, OpenBrowserError::START_FAILED) => {
            tr("The browser couldn’t start. Check tufo-browser.log beside the server’s log.")
        }
        (Code::NotFound, PlayAudioError::NOT_FOUND)
        | (
            Code::FailedPrecondition,
            PlayEntrySoundError::MISSING | AddMusicLayerError::MISSING | AddToQueueError::MISSING,
        ) => tr("The audio file is missing. Restore it and try again."),
        (Code::NotFound, AddMusicLayerError::NOT_FOUND | AddToQueueError::NOT_FOUND) => {
            tr("That track or sound no longer exists.")
        }
        (Code::FailedPrecondition, AddToQueueError::REPEATING_TRACK) => {
            tr("Stop repeating the song to queue others.")
        }
        (Code::FailedPrecondition, ControlMusicError::SONGS_QUEUED) => {
            tr("Remove the queued songs to repeat one song.")
        }
        (Code::FailedPrecondition, AddMusicLayerError::FULL) => {
            tr("The mix has three songs. Stop a layer before adding another.")
        }
        (Code::FailedPrecondition, PlayAudioError::UNREADABLE) => {
            tr("Could not read the audio file. Check its permissions.")
        }
        (Code::FailedPrecondition, PlayAudioError::UNSUPPORTED) => {
            tr("This audio format can't be played.")
        }
        (Code::Unavailable, PlayAudioError::OUTPUT_UNAVAILABLE) => {
            tr("The audio output is unavailable. Choose another output.")
        }
        (Code::FailedPrecondition, ControlMusicError::NOTHING_PLAYING) => {
            tr("No music is playing.")
        }
        (Code::FailedPrecondition, ControlMusicError::SEEK_PAST_END) => {
            tr("That position is past the end of the track.")
        }
        (
            Code::FailedPrecondition,
            SelectOutputError::DISCORD_NOT_CONFIGURED | DiscordClientError::NOT_CONFIGURED,
        ) => tr("Add a Discord bot token in Settings first."),
        (
            Code::FailedPrecondition,
            SelectOutputError::DISCORD_DISABLED | DiscordClientError::DISABLED,
        ) => tr("The Discord connection is disabled. Enable it in Settings first."),
        (Code::NotFound, SelectOutputError::UNKNOWN_VOICE_CHANNEL) => {
            tr("That voice channel is no longer available. Refresh the channels.")
        }
        (Code::Unavailable, SelectOutputError::VOICE_CONNECTION) => {
            tr("Could not join the voice channel. Check the bot’s permissions and try again.")
        }
        (Code::Unavailable, SelectOutputError::LOCAL_DEVICE) => {
            tr("Could not open the audio device. Check your system’s audio settings.")
        }
        (Code::Unavailable, DiscordClientError::NOT_READY) => {
            tr("The Discord bot is still connecting. Try again shortly.")
        }
        (Code::InvalidArgument, SetDiscordBotTokenError::EMPTY_TOKEN) => {
            tr("Paste a Discord bot token before saving.")
        }
        (Code::Internal, CredentialsError::REASON) => {
            tr("Could not read or save the Discord bot token. Check the config folder permissions.")
        }
        _ => return None,
    })
}

pub(crate) fn kenku_report_text(report: &KenkuImportReport) -> String {
    let mut sentences = Vec::new();
    if report.collections_imported > 0 {
        sentences.push(tr_args(
            "Imported {0} with {1} from Kenku FM.",
            &[
                &tr_count(
                    "{n} collection",
                    "{n} collections",
                    report.collections_imported,
                ),
                &tr_count("{n} file", "{n} files", report.files_imported),
            ],
        ));
    }
    if !report.skipped_collections.is_empty() {
        sentences.push(tr_count(
            "{n} collection was already in the library.",
            "{n} collections were already in the library.",
            report.skipped_collections.len(),
        ));
    }
    if !report.unreadable_files.is_empty() {
        sentences.push(tr_count(
            "{n} file could not be found or read.",
            "{n} files could not be found or read.",
            report.unreadable_files.len(),
        ));
    }
    if !report.unsupported_sources.is_empty() {
        sentences.push(tr_count(
            "Left out {n} web address that cannot be played.",
            "Left out {n} web addresses that cannot be played.",
            report.unsupported_sources.len(),
        ));
    }
    if report.discord_bot_token_imported {
        sentences.push(tr("Discord bot token imported.").to_owned());
    }
    if sentences.is_empty() {
        return tr("Kenku FM's library is empty.").to_owned();
    }
    sentences.join(" ")
}

pub(crate) fn kenku_error_text(reason: &str) -> String {
    use tufo_application::library::import_kenku_library::{INTERRUPTED, TIMED_OUT};
    match reason {
        INTERRUPTED => tr("Import was interrupted. Retry to continue."),
        TIMED_OUT => tr("Import took too long. Retry to continue."),
        _ => library_reason_text(Code::FailedPrecondition, reason)
            .or_else(|| library_reason_text(Code::NotFound, reason))
            .or_else(|| reason_text(Code::Internal, reason))
            .unwrap_or_else(|| tr("Could not complete the request. Check the server log.")),
    }
    .to_owned()
}

#[cfg(test)]
mod tests;
