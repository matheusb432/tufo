//! Audio files Tufo points at, and the playlists and soundboards that collect them.
//!
//! Tufo never copies, moves, or writes audio files. An [`AudioFile`] row remembers where a file
//! was and enough about it to find it again after it moves: its [`FileStamp`] and its
//! [`AudioFingerprint`].

use std::{cmp::Ordering, fmt, time::Duration};

use jiff::Timestamp;
use nutype::nutype;

use crate::{
    path::{FolderPath, ImageFilePath},
    playback::AudioFilePath,
    storage::{checked_column, uuid_identifier},
};

pub mod default_effects;
pub mod song_loop;

/// A Tufo-generated title, translated by the GUI until the user renames it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, toasty::Embed)]
pub enum BuiltinTitle {
    Effects,
    Imported,
    Rain,
    Storm,
    Wind,
    Waves,
    Stream,
    Birds,
    SummerNight,
    Fireplace,
}

uuid_identifier!(
    /// One audio file on disk, wherever it currently lives.
    AudioFileId
);
uuid_identifier!(
    /// A folder dropped into Tufo, which Tufo watches recursively.
    LibraryFolderId
);
uuid_identifier!(
    /// One playlist or soundboard.
    CollectionId
);
uuid_identifier!(
    /// One audio file's place in one collection.
    CollectionEntryId
);

/// Whether a collection holds music or sound effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, toasty::Embed)]
pub enum CollectionKind {
    /// Tracks played one after another, with crossfades.
    Playlist,
    /// Sounds fired over the music, several at once.
    Soundboard,
}

/// A linear gain from silence (0) to the file's own level (1).
#[nutype(
    validate(finite, greater_or_equal = 0.0, less_or_equal = 1.0),
    default = 1.0,
    derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)
)]
pub struct Volume(f32);

/// The longest fade Tufo accepts.
pub const FADE_DURATION_MAX: Duration = Duration::from_mins(1);

/// How long a sound takes to fade in or out, at most [`FADE_DURATION_MAX`].
#[nutype(
    validate(predicate = |duration| *duration <= FADE_DURATION_MAX),
    default = Duration::from_millis(100),
    derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)
)]
pub struct FadeDuration(Duration);

/// How one sound plays on its soundboard. The defaults are Kenku FM's.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SoundSettings {
    pub looping: bool,
    pub volume: Volume,
    pub fade_in: FadeDuration,
    pub fade_out: FadeDuration,
}

pub const TITLE_LENGTH_MAX: usize = 200;
const UNTITLED: &str = "Untitled";

/// A display name: trimmed, not empty, at most [`TITLE_LENGTH_MAX`] characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Title(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TitleError {
    #[error("the title is empty")]
    Empty,
    #[error("the title is longer than {TITLE_LENGTH_MAX} characters")]
    TooLong,
}

impl Title {
    /// Trims `text` and accepts it when it's not empty and fits the limit.
    pub fn try_new(text: String) -> Result<Self, TitleError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(TitleError::Empty);
        }
        if trimmed.chars().count() > TITLE_LENGTH_MAX {
            return Err(TitleError::TooLong);
        }
        let trimmed_copy = (trimmed.len() != text.len()).then(|| trimmed.to_owned());
        Ok(Self(trimmed_copy.unwrap_or(text)))
    }

    /// A title made from a name Tufo found on disk: trimmed, cut to the limit, and "Untitled"
    /// when nothing is left.
    #[must_use]
    pub fn from_name_lossy(name: &str) -> Self {
        let cut = name
            .trim()
            .chars()
            .take(TITLE_LENGTH_MAX)
            .collect::<String>();
        match cut.trim_end() {
            "" => Self(UNTITLED.to_owned()),
            title => Self(title.to_owned()),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Title {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Title {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Compares titles the way people read them: case-insensitively, with runs of digits compared
/// as numbers, so "Track 2" comes before "Track 10".
#[must_use]
pub fn compare_titles(left: &str, right: &str) -> Ordering {
    TitleChunks::new(left)
        .cmp(TitleChunks::new(right))
        .then_with(|| left.cmp(right))
}

/// A title split into runs of digits, compared by value, and single lowercase characters.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum TitleChunk {
    /// Leading zeros are dropped; the digit count then orders numbers of any length.
    Number {
        digit_count: usize,
        digits: String,
    },
    Character(char),
}

struct TitleChunks<'a> {
    rest: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> TitleChunks<'a> {
    fn new(title: &'a str) -> Self {
        Self {
            rest: title.chars().peekable(),
        }
    }
}

impl Iterator for TitleChunks<'_> {
    type Item = TitleChunk;

    fn next(&mut self) -> Option<TitleChunk> {
        let first = self.rest.next()?;
        if !first.is_ascii_digit() {
            return Some(TitleChunk::Character(
                first.to_lowercase().next().unwrap_or(first),
            ));
        }
        let mut digits = String::from(first);
        while let Some(digit) = self.rest.next_if(char::is_ascii_digit) {
            digits.push(digit);
        }
        let significant = digits.trim_start_matches('0').to_owned();
        Some(TitleChunk::Number {
            digit_count: significant.len(),
            digits: significant,
        })
    }
}

pub const FILE_IDENTITY_LENGTH_MAX: usize = 100;

/// What the operating system calls a file independently of its path: the device and inode on
/// Unix, or the volume serial number and file index on Windows. It survives renames and moves
/// within one volume, and nothing else.
#[nutype(
    validate(not_empty, len_char_max = FILE_IDENTITY_LENGTH_MAX),
    derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, AsRef, Display)
)]
pub struct FileIdentity(String);

/// The filesystem facts that reveal whether a file changed since Tufo last read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStamp {
    pub identity: FileIdentity,
    pub size_bytes: u64,
    /// The modification time in nanoseconds since the Unix epoch, negative before it.
    pub modified_ns: i64,
}

const FINGERPRINT_HEX_LENGTH: usize = 64;

/// A BLAKE3 hash of an audio file's compressed audio packets, in lowercase hex. Tag edits
/// don't change it, so it finds a file again after a move that changed its file identity.
#[nutype(
    validate(predicate = |hex| hex.len() == FINGERPRINT_HEX_LENGTH
        && hex.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))),
    derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, AsRef, Display)
)]
pub struct AudioFingerprint(String);

/// A SHA-256 hash of a whole file, in lowercase hex. Unlike [`AudioFingerprint`], tag edits change
/// it.
#[nutype(
    validate(predicate = |hex| hex.len() == FINGERPRINT_HEX_LENGTH
        && hex.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))),
    derive(Debug, Clone, PartialEq, Eq, Hash, AsRef, Display)
)]
pub struct FileSha256(String);

/// Rounds `duration` down to whole milliseconds, saturating at about 49 days, which is how Tufo
/// stores durations.
#[must_use]
pub fn milliseconds_saturating(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

/// Which property orders a collection's entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, toasty::Embed)]
pub enum EntryOrderKey {
    #[default]
    Title,
    Duration,
    /// When the file was added to Tufo.
    DateAdded,
}

/// How a collection orders its entries. Ties fall back to the title, then the entry's id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EntryOrder {
    pub key: EntryOrderKey,
    pub descending: bool,
}

/// One audio file Tufo knows, at the path where Tufo last saw it.
#[derive(Debug, toasty::Model)]
pub struct AudioFile {
    #[key]
    #[auto]
    pub id: AudioFileId,
    #[unique]
    pub file_path: AudioFilePath,
    #[index]
    pub file_identity: FileIdentity,
    pub size_bytes: i64,
    pub modified_ns: i64,
    #[index]
    pub fingerprint: AudioFingerprint,
    pub duration_ms: u32,
    pub title: Title,
    pub builtin_title: Option<BuiltinTitle>,
    /// When Tufo last looked for the file and didn't find it.
    pub missing_since: Option<Timestamp>,
    /// When the file was added to Tufo; the "date added" order.
    pub created_at: Timestamp,
    /// Added to a playlist by its own path, so it stays among the songs, wherever it moves,
    /// until removed from them.
    pub song_source: bool,
}

impl AudioFile {
    /// The stored stamp. Sizes past `i64::MAX` can't occur on real filesystems, so a negative
    /// stored size reads as zero, which never matches a scanned file.
    #[must_use]
    pub fn stamp(&self) -> FileStamp {
        FileStamp {
            identity: self.file_identity.clone(),
            size_bytes: u64::try_from(self.size_bytes).unwrap_or(0),
            modified_ns: self.modified_ns,
        }
    }

    #[must_use]
    pub fn duration(&self) -> Duration {
        Duration::from_millis(u64::from(self.duration_ms))
    }
}

/// A folder dropped on a grid. Tufo watches it recursively, and each folder under it that holds
/// audio becomes a linked collection of the same kind.
#[derive(Debug, toasty::Model)]
pub struct LibraryFolder {
    #[key]
    #[auto]
    pub id: LibraryFolderId,
    pub kind: CollectionKind,
    pub folder_path: FolderPath,
    pub folder_identity: FileIdentity,
    pub created_at: Timestamp,
}

/// A public-domain painting that ships with Tufo as a collection background. The GUI holds the
/// images; the library stores only which one a collection shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, toasty::Embed)]
pub enum BackgroundPreset {
    Autumn,
    Battle,
    Castle,
    Cathedral,
    Coast,
    Desert,
    Dragon,
    Fire,
    Forest,
    Haunted,
    Mountain,
    Night,
    Sky,
    Skulls,
    Storm,
    Tavern,
    Town,
    Underworld,
    Volcano,
    Winter,
}

impl BackgroundPreset {
    pub const ALL: [Self; 20] = [
        Self::Autumn,
        Self::Battle,
        Self::Castle,
        Self::Cathedral,
        Self::Coast,
        Self::Desert,
        Self::Dragon,
        Self::Fire,
        Self::Forest,
        Self::Haunted,
        Self::Mountain,
        Self::Night,
        Self::Sky,
        Self::Skulls,
        Self::Storm,
        Self::Tavern,
        Self::Town,
        Self::Underworld,
        Self::Volcano,
        Self::Winter,
    ];

    /// The preset's position in [`Self::ALL`].
    #[must_use]
    pub fn index(self) -> usize {
        self as usize
    }
}

macro_rules! sound_icons {
    ($($variant:ident),+ $(,)?) => {
        /// A Lucide icon for a sound's tile. The GUI holds the drawings.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, toasty::Embed)]
        pub enum SoundIcon {
            #[default]
            $($variant),+
        }

        impl SoundIcon {
            /// Every icon, in declaration order: the picker's order.
            pub const ALL: &[Self] = &[$(Self::$variant),+];
        }
    };
}

sound_icons!(
    AudioLines,
    Anchor,
    Axe,
    Beer,
    Bell,
    Bird,
    Bomb,
    Bone,
    BowArrow,
    Bug,
    Building,
    Castle,
    Cat,
    Church,
    Clock,
    CloudFog,
    CloudLightning,
    CloudRain,
    CloudSnow,
    Coffee,
    Coins,
    Crown,
    Dices,
    Dog,
    DoorOpen,
    Droplets,
    Drum,
    Fan,
    Fish,
    Flame,
    FlameKindling,
    FlaskConical,
    Footprints,
    Ghost,
    Guitar,
    Hammer,
    HeartPulse,
    Hourglass,
    House,
    Key,
    Landmark,
    Moon,
    MoonStar,
    Mountain,
    MountainSnow,
    Music,
    PawPrint,
    Piano,
    Pickaxe,
    Rabbit,
    Rat,
    Scroll,
    Shield,
    Ship,
    Siren,
    Skull,
    Snowflake,
    Sparkles,
    Squirrel,
    Sun,
    Swords,
    Tent,
    Tornado,
    TrainFront,
    Trees,
    VenetianMask,
    WandSparkles,
    Waves,
    Wind,
    Zap,
);

impl SoundIcon {
    /// The icon's position in [`Self::ALL`].
    #[must_use]
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Picks presets for new collections: each pick is random among the presets the fewest
/// collections show, so the grid repeats a painting only after it shows every one.
#[derive(Debug, Clone)]
pub struct BackgroundPresetChooser {
    collection_counts: [usize; BackgroundPreset::ALL.len()],
    rng: fastrand::Rng,
}

impl BackgroundPresetChooser {
    /// Counts the presets `shown` by the existing collections.
    #[must_use]
    pub fn new(shown: impl IntoIterator<Item = BackgroundPreset>, rng: fastrand::Rng) -> Self {
        let mut collection_counts = [0; BackgroundPreset::ALL.len()];
        for preset in shown {
            collection_counts[preset.index()] += 1;
        }
        Self {
            collection_counts,
            rng,
        }
    }

    /// Picks a preset for one new collection and counts it as shown.
    pub fn choose(&mut self) -> BackgroundPreset {
        let fewest = self.collection_counts.iter().copied().min().unwrap_or(0);
        let least_shown = BackgroundPreset::ALL
            .into_iter()
            .filter(|preset| self.collection_counts[preset.index()] == fewest)
            .collect::<Vec<_>>();
        let preset = self
            .rng
            .choice(least_shown)
            .unwrap_or(BackgroundPreset::ALL[0]);
        self.collection_counts[preset.index()] += 1;
        preset
    }
}

/// A playlist or soundboard. A linked collection mirrors one folder under a library folder; a
/// manual collection holds whatever you add to it.
#[derive(Debug, toasty::Model)]
pub struct Collection {
    #[key]
    #[auto]
    pub id: CollectionId,
    pub kind: CollectionKind,
    pub title: Title,
    pub builtin_title: Option<BuiltinTitle>,
    /// At most one of the preset and the image is present; with neither, the collection shows
    /// its gradient.
    pub background_preset: Option<BackgroundPreset>,
    pub background_image: Option<ImageFilePath>,
    pub order_key: EntryOrderKey,
    pub order_descending: bool,
    #[index]
    pub library_folder_id: Option<LibraryFolderId>,
    /// The linked folder, present exactly when `library_folder_id` is.
    pub folder_path: Option<FolderPath>,
    pub folder_identity: Option<FileIdentity>,
    /// When Tufo last looked for the linked folder and didn't find it.
    pub folder_missing_since: Option<Timestamp>,
    /// When you deleted this linked collection. The row stays so rescans don't recreate it.
    pub hidden_at: Option<Timestamp>,
    pub created_at: Timestamp,
    /// The sidebar lists it.
    pub pinned: bool,
}

impl Collection {
    #[must_use]
    pub fn order(&self) -> EntryOrder {
        EntryOrder {
            key: self.order_key,
            descending: self.order_descending,
        }
    }
}

/// One audio file's place in one collection, with how it plays there as a sound and the icon its
/// tile shows.
#[derive(Debug, toasty::Model)]
pub struct CollectionEntry {
    #[key]
    #[auto]
    pub id: CollectionEntryId,
    #[index]
    pub collection_id: CollectionId,
    #[index]
    pub audio_file_id: AudioFileId,
    pub added_at: Timestamp,
    pub looping: bool,
    pub volume: Volume,
    pub fade_in: FadeDuration,
    pub fade_out: FadeDuration,
    pub icon: SoundIcon,
}

impl CollectionEntry {
    #[must_use]
    pub fn sound_settings(&self) -> SoundSettings {
        SoundSettings {
            looping: self.looping,
            volume: self.volume,
            fade_in: self.fade_in,
            fade_out: self.fade_out,
        }
    }
}

checked_column!(
    Title,
    String,
    decode = Title::try_new,
    encode = |title| title.as_ref().to_owned(),
);
checked_column!(
    FileIdentity,
    String,
    decode = FileIdentity::try_new,
    encode = |identity| identity.as_ref().to_owned(),
);
checked_column!(
    AudioFingerprint,
    String,
    decode = AudioFingerprint::try_new,
    encode = |fingerprint| fingerprint.as_ref().to_owned(),
);
checked_column!(
    Volume,
    f32,
    decode = Volume::try_new,
    encode = |volume| volume.into_inner(),
);
checked_column!(
    FadeDuration,
    u32,
    decode = |milliseconds| FadeDuration::try_new(Duration::from_millis(u64::from(milliseconds))),
    encode = |fade| milliseconds_saturating(fade.into_inner()),
);

#[cfg(test)]
mod tests {
    use toasty::{
        schema::Load as _,
        stmt::{Uuid, Value},
    };

    use super::*;

    #[test]
    fn fades_are_capped_at_one_minute() {
        assert!(FadeDuration::try_new(FADE_DURATION_MAX).is_ok());
        assert!(FadeDuration::try_new(FADE_DURATION_MAX + Duration::from_millis(1)).is_err());
    }

    #[test]
    fn names_from_disk_always_make_a_title() {
        assert_eq!(Title::from_name_lossy("  Tavern  ").as_str(), "Tavern");
        assert_eq!(Title::from_name_lossy("   ").as_str(), "Untitled");
        assert_eq!(
            Title::from_name_lossy(&"a".repeat(TITLE_LENGTH_MAX + 5))
                .as_str()
                .len(),
            TITLE_LENGTH_MAX
        );
    }

    #[test]
    fn fingerprints_are_lowercase_blake3_hex() {
        let valid = "0123456789abcdef".repeat(4);

        assert!(AudioFingerprint::try_new(valid.clone()).is_ok());
        assert!(AudioFingerprint::try_new(valid.to_uppercase()).is_err());
        assert!(AudioFingerprint::try_new(valid[1..].to_owned()).is_err());
    }

    #[test]
    fn stored_values_decode_through_their_constructors() {
        assert!(Volume::load(Value::from(1.5_f32)).is_err());
        assert!(FadeDuration::load(Value::from(60_001_u32)).is_err());
        assert!(Title::load(Value::from("   ")).is_err());
        assert_eq!(
            Title::load(Value::from("  Tavern  ")).unwrap().as_ref(),
            "Tavern"
        );
    }

    #[test]
    fn presets_sit_at_their_index() {
        for (index, preset) in BackgroundPreset::ALL.into_iter().enumerate() {
            assert_eq!(preset.index(), index);
        }
    }

    #[test]
    fn sound_icons_sit_at_their_index() {
        for (index, icon) in SoundIcon::ALL.iter().enumerate() {
            assert_eq!(icon.index(), index);
        }
    }

    #[test]
    fn new_collections_get_every_preset_before_one_repeats() {
        let mut chooser = BackgroundPresetChooser::new([], fastrand::Rng::with_seed(7));

        let mut first_round = (0..BackgroundPreset::ALL.len())
            .map(|_| chooser.choose())
            .collect::<Vec<_>>();
        first_round.sort();

        assert_eq!(first_round, BackgroundPreset::ALL);
    }

    #[test]
    fn presets_other_collections_show_come_last() {
        let shown = BackgroundPreset::ALL
            .into_iter()
            .filter(|preset| *preset != BackgroundPreset::Tavern);
        let mut chooser = BackgroundPresetChooser::new(shown, fastrand::Rng::with_seed(7));

        assert_eq!(chooser.choose(), BackgroundPreset::Tavern);
    }

    #[test]
    fn identifiers_display_as_hyphenated_uuids() {
        let uuid = Uuid::parse_str("01a0cf81-04dd-7d42-9a53-2904e3318bc7").unwrap();

        assert_eq!(
            CollectionId::from_uuid(uuid).to_string(),
            "01a0cf81-04dd-7d42-9a53-2904e3318bc7"
        );
    }
}
