//! Protobuf mappings for the library.

use std::path::PathBuf;

use jiff::Timestamp;
use tonic::Status;
use tufo_models::{
    library::{
        AudioFileId, BackgroundPreset, BuiltinTitle, CollectionEntryId, CollectionId,
        CollectionKind, EntryOrder, EntryOrderKey, FadeDuration, SoundIcon, SoundSettings, Title,
        Volume,
    },
    path::{FolderPath, ImageFilePath, LocalPath},
    playback::AudioFilePath,
};

use super::{InvalidMessage, duration_message, duration_value, invalid, parse_uuid, required};
use crate::{
    library::{
        AddFilesToCollection, Background, CollectionSource, CollectionView, CreateCollection,
        EntryView, ImportKenkuLibrary, ImportPaths, ImportReport, KenkuImportReport,
        LibrarySnapshot, UpdateCollection, UpdateCollectionEntry,
    },
    pb,
};

// Requests the server parses.

pub fn import_paths_request(request: pb::ImportPathsRequest) -> Result<ImportPaths, Status> {
    Ok(ImportPaths {
        kind: collection_kind(request.kind).map_err(|reason| invalid("kind", reason))?,
        paths: local_paths(request.paths)?,
    })
}

pub fn import_kenku_library_request(
    request: pb::ImportKenkuLibraryRequest,
) -> Result<ImportKenkuLibrary, Status> {
    let kenku_folder = request
        .kenku_folder_path
        .map(|path| {
            FolderPath::try_new(PathBuf::from(path))
                .map_err(|_| invalid("kenku_folder_path", "must be absolute"))
        })
        .transpose()?;
    Ok(ImportKenkuLibrary { kenku_folder })
}

pub fn add_files_to_collection_request(
    request: pb::AddFilesToCollectionRequest,
) -> Result<AddFilesToCollection, Status> {
    Ok(AddFilesToCollection {
        collection_id: collection_id(&request.collection_id)?,
        paths: local_paths(request.paths)?,
    })
}

pub fn create_collection_request(
    request: pb::CreateCollectionRequest,
) -> Result<CreateCollection, Status> {
    Ok(CreateCollection {
        kind: collection_kind(request.kind).map_err(|reason| invalid("kind", reason))?,
        title: Title::try_new(request.title).map_err(|error| invalid("title", error))?,
        background: request
            .background
            .ok_or("is required")
            .and_then(background_value)
            .map_err(|reason| invalid("background", reason))?,
    })
}

pub fn update_collection_request(
    request: pb::UpdateCollectionRequest,
) -> Result<(CollectionId, UpdateCollection), Status> {
    let update = UpdateCollection {
        title: request
            .title
            .map(|title| Title::try_new(title).map_err(|error| invalid("title", error)))
            .transpose()?,
        background: request
            .background
            .map(|background| {
                background_value(background).map_err(|reason| invalid("background", reason))
            })
            .transpose()?,
        order: request
            .order
            .map(|order| entry_order(order).map_err(|reason| invalid("order", reason)))
            .transpose()?,
    };
    Ok((collection_id(&request.collection_id)?, update))
}

pub fn delete_collection_request(
    request: &pb::DeleteCollectionRequest,
) -> Result<CollectionId, Status> {
    collection_id(&request.collection_id)
}

pub fn update_collection_entry_request(
    request: pb::UpdateCollectionEntryRequest,
) -> Result<(CollectionEntryId, UpdateCollectionEntry), Status> {
    let update = UpdateCollectionEntry {
        title: request
            .title
            .map(|title| Title::try_new(title).map_err(|error| invalid("title", error)))
            .transpose()?,
        sound: request
            .sound
            .map(|sound| sound_settings(sound).map_err(|reason| invalid("sound", reason)))
            .transpose()?,
        icon: request
            .icon
            .map(|icon| sound_icon(icon).map_err(|reason| invalid("icon", reason)))
            .transpose()?,
    };
    Ok((collection_entry_id(&request.collection_entry_id)?, update))
}

pub fn delete_collection_entry_request(
    request: &pb::DeleteCollectionEntryRequest,
) -> Result<CollectionEntryId, Status> {
    collection_entry_id(&request.collection_entry_id)
}

// Responses the server builds.

#[must_use]
pub fn watch_library_response(snapshot: &LibrarySnapshot) -> pb::WatchLibraryResponse {
    pb::WatchLibraryResponse {
        snapshot: Some(pb::LibrarySnapshot {
            revision: snapshot.revision,
            collections: snapshot
                .collections
                .iter()
                .map(collection_message)
                .collect(),
        }),
    }
}

#[must_use]
pub fn import_report_message(report: &ImportReport) -> pb::ImportReport {
    pb::ImportReport {
        collections_created: u32::try_from(report.collections_created).unwrap_or(u32::MAX),
        files_added: u32::try_from(report.files_added).unwrap_or(u32::MAX),
        unreadable_paths: report
            .unreadable_files
            .iter()
            .map(|path| path_text(path.as_ref()))
            .collect(),
        ignored_paths: report
            .ignored_paths
            .iter()
            .map(|path| path_text(path.as_ref()))
            .collect(),
    }
}

#[must_use]
pub fn kenku_import_report_message(report: &KenkuImportReport) -> pb::KenkuImportReport {
    pb::KenkuImportReport {
        collections_imported: u32::try_from(report.collections_imported).unwrap_or(u32::MAX),
        files_imported: u32::try_from(report.files_imported).unwrap_or(u32::MAX),
        skipped_collection_titles: report
            .skipped_collections
            .iter()
            .map(|title| title.as_str().to_owned())
            .collect(),
        unreadable_paths: report
            .unreadable_files
            .iter()
            .map(|path| path_text(path.as_ref()))
            .collect(),
        unsupported_sources: report.unsupported_sources.clone(),
        discord_bot_token_imported: report.discord_bot_token_imported,
    }
}

#[must_use]
pub fn create_collection_response(id: CollectionId) -> pb::CreateCollectionResponse {
    pb::CreateCollectionResponse {
        collection_id: id.to_string(),
    }
}

// Requests the client builds.

#[must_use]
pub fn import_paths_message(request: &ImportPaths) -> pb::ImportPathsRequest {
    pb::ImportPathsRequest {
        kind: collection_kind_message(request.kind).into(),
        paths: request
            .paths
            .iter()
            .map(|path| path_text(path.as_ref()))
            .collect(),
    }
}

#[must_use]
pub fn import_kenku_library_message(request: &ImportKenkuLibrary) -> pb::ImportKenkuLibraryRequest {
    pb::ImportKenkuLibraryRequest {
        kenku_folder_path: request
            .kenku_folder
            .as_ref()
            .map(|folder| path_text(folder.as_ref())),
    }
}

#[must_use]
pub fn add_files_to_collection_message(
    request: &AddFilesToCollection,
) -> pb::AddFilesToCollectionRequest {
    pb::AddFilesToCollectionRequest {
        collection_id: request.collection_id.to_string(),
        paths: request
            .paths
            .iter()
            .map(|path| path_text(path.as_ref()))
            .collect(),
    }
}

#[must_use]
pub fn create_collection_message(request: &CreateCollection) -> pb::CreateCollectionRequest {
    pb::CreateCollectionRequest {
        kind: collection_kind_message(request.kind).into(),
        title: request.title.as_str().to_owned(),
        background: Some(background_message(&request.background)),
    }
}

#[must_use]
pub fn update_collection_message(
    id: CollectionId,
    update: &UpdateCollection,
) -> pb::UpdateCollectionRequest {
    pb::UpdateCollectionRequest {
        collection_id: id.to_string(),
        title: update.title.as_ref().map(|title| title.as_str().to_owned()),
        background: update.background.as_ref().map(background_message),
        order: update.order.map(entry_order_message),
    }
}

#[must_use]
pub fn delete_collection_message(id: CollectionId) -> pb::DeleteCollectionRequest {
    pb::DeleteCollectionRequest {
        collection_id: id.to_string(),
    }
}

#[must_use]
pub fn update_collection_entry_message(
    id: CollectionEntryId,
    update: &UpdateCollectionEntry,
) -> pb::UpdateCollectionEntryRequest {
    pb::UpdateCollectionEntryRequest {
        collection_entry_id: id.to_string(),
        title: update.title.as_ref().map(|title| title.as_str().to_owned()),
        sound: update.sound.map(sound_settings_message),
        icon: update.icon.map(|icon| sound_icon_message(icon).into()),
    }
}

#[must_use]
pub fn delete_collection_entry_message(id: CollectionEntryId) -> pb::DeleteCollectionEntryRequest {
    pb::DeleteCollectionEntryRequest {
        collection_entry_id: id.to_string(),
    }
}

// Responses the client decodes.

pub fn library_snapshot(
    response: pb::WatchLibraryResponse,
) -> Result<LibrarySnapshot, InvalidMessage> {
    let snapshot = response
        .snapshot
        .ok_or(InvalidMessage { field: "snapshot" })?;
    Ok(LibrarySnapshot {
        revision: snapshot.revision,
        collections: snapshot
            .collections
            .into_iter()
            .map(collection_view)
            .collect::<Result<_, _>>()?,
    })
}

pub fn import_report(report: Option<pb::ImportReport>) -> Result<ImportReport, InvalidMessage> {
    let report = report.ok_or(InvalidMessage { field: "report" })?;
    let unreadable_files = report
        .unreadable_paths
        .into_iter()
        .map(|path| {
            AudioFilePath::try_new(PathBuf::from(path)).map_err(|_| InvalidMessage {
                field: "unreadable_paths",
            })
        })
        .collect::<Result<_, _>>()?;
    let ignored_paths = report
        .ignored_paths
        .into_iter()
        .map(|path| {
            LocalPath::try_new(PathBuf::from(path)).map_err(|_| InvalidMessage {
                field: "ignored_paths",
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(ImportReport {
        collections_created: usize::try_from(report.collections_created).unwrap_or(usize::MAX),
        files_added: usize::try_from(report.files_added).unwrap_or(usize::MAX),
        unreadable_files,
        ignored_paths,
    })
}

pub fn kenku_import_report(
    report: Option<pb::KenkuImportReport>,
) -> Result<KenkuImportReport, InvalidMessage> {
    let report = report.ok_or(InvalidMessage { field: "report" })?;
    let skipped_collections = report
        .skipped_collection_titles
        .into_iter()
        .map(|title| {
            Title::try_new(title).map_err(|_| InvalidMessage {
                field: "skipped_collection_titles",
            })
        })
        .collect::<Result<_, _>>()?;
    let unreadable_files = report
        .unreadable_paths
        .into_iter()
        .map(|path| {
            AudioFilePath::try_new(PathBuf::from(path)).map_err(|_| InvalidMessage {
                field: "unreadable_paths",
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(KenkuImportReport {
        collections_imported: usize::try_from(report.collections_imported).unwrap_or(usize::MAX),
        files_imported: usize::try_from(report.files_imported).unwrap_or(usize::MAX),
        skipped_collections,
        unreadable_files,
        unsupported_sources: report.unsupported_sources,
        discord_bot_token_imported: report.discord_bot_token_imported,
    })
}

pub fn created_collection_id(
    response: &pb::CreateCollectionResponse,
) -> Result<CollectionId, InvalidMessage> {
    parse_uuid(&response.collection_id)
        .map(CollectionId::from_uuid)
        .ok_or(InvalidMessage {
            field: "collection_id",
        })
}

// Shared conversions.

fn collection_message(collection: &CollectionView) -> pb::Collection {
    let source = match &collection.source {
        CollectionSource::Manual => pb::collection::Source::Manual(pb::ManualSource {}),
        CollectionSource::Linked { folder, missing } => {
            pb::collection::Source::Linked(pb::LinkedSource {
                folder_path: path_text(folder.as_ref()),
                missing: *missing,
            })
        }
    };
    pb::Collection {
        id: collection.id.to_string(),
        kind: collection_kind_message(collection.kind).into(),
        title: collection.title.as_str().to_owned(),
        builtin_title: collection.builtin_title.map(builtin_title_message),
        background: Some(background_message(&collection.background)),
        order: Some(entry_order_message(collection.order)),
        source: Some(source),
        entries: collection.entries.iter().map(entry_message).collect(),
    }
}

fn entry_message(entry: &EntryView) -> pb::CollectionEntry {
    pb::CollectionEntry {
        id: entry.id.to_string(),
        audio_file_id: entry.audio_file_id.to_string(),
        title: entry.title.as_str().to_owned(),
        builtin_title: entry.builtin_title.map(builtin_title_message),
        path: path_text(entry.path.as_ref()),
        duration: Some(duration_message(entry.duration)),
        added_at: Some(prost_types::Timestamp {
            seconds: entry.added_at.as_second(),
            nanos: entry.added_at.subsec_nanosecond(),
        }),
        missing: entry.missing,
        sound: Some(sound_settings_message(entry.sound)),
        icon: sound_icon_message(entry.icon).into(),
    }
}

fn collection_view(message: pb::Collection) -> Result<CollectionView, InvalidMessage> {
    let invalid = |field| InvalidMessage { field };
    let source = match message.source.ok_or(invalid("source"))? {
        pb::collection::Source::Manual(_) => CollectionSource::Manual,
        pb::collection::Source::Linked(linked) => CollectionSource::Linked {
            folder: FolderPath::try_new(PathBuf::from(linked.folder_path))
                .map_err(|_| invalid("folder_path"))?,
            missing: linked.missing,
        },
    };
    Ok(CollectionView {
        id: parse_uuid(&message.id)
            .map(CollectionId::from_uuid)
            .ok_or(invalid("id"))?,
        kind: collection_kind(message.kind).map_err(|_| invalid("kind"))?,
        title: Title::try_new(message.title).map_err(|_| invalid("title"))?,
        builtin_title: message.builtin_title.map(builtin_title).transpose()?,
        background: message
            .background
            .ok_or("is required")
            .and_then(background_value)
            .map_err(|_| invalid("background"))?,
        order: message
            .order
            .ok_or("is required")
            .and_then(entry_order)
            .map_err(|_| invalid("order"))?,
        source,
        entries: message
            .entries
            .into_iter()
            .map(entry_view)
            .collect::<Result<_, _>>()?,
    })
}

fn entry_view(message: pb::CollectionEntry) -> Result<EntryView, InvalidMessage> {
    let invalid = |field| InvalidMessage { field };
    let added_at = message.added_at.ok_or(invalid("added_at"))?;
    Ok(EntryView {
        id: parse_uuid(&message.id)
            .map(CollectionEntryId::from_uuid)
            .ok_or(invalid("id"))?,
        audio_file_id: parse_uuid(&message.audio_file_id)
            .map(AudioFileId::from_uuid)
            .ok_or(invalid("audio_file_id"))?,
        title: Title::try_new(message.title).map_err(|_| invalid("title"))?,
        builtin_title: message.builtin_title.map(builtin_title).transpose()?,
        path: AudioFilePath::try_new(PathBuf::from(message.path)).map_err(|_| invalid("path"))?,
        duration: message
            .duration
            .and_then(duration_value)
            .ok_or(invalid("duration"))?,
        added_at: Timestamp::new(added_at.seconds, added_at.nanos)
            .map_err(|_| invalid("added_at"))?,
        missing: message.missing,
        sound: message
            .sound
            .ok_or("is required")
            .and_then(sound_settings)
            .map_err(|_| invalid("sound"))?,
        icon: sound_icon(message.icon).map_err(|_| invalid("icon"))?,
    })
}

fn builtin_title(value: i32) -> Result<BuiltinTitle, InvalidMessage> {
    match pb::BuiltinTitle::try_from(value) {
        Ok(pb::BuiltinTitle::Effects) => Ok(BuiltinTitle::Effects),
        Ok(pb::BuiltinTitle::Imported) => Ok(BuiltinTitle::Imported),
        Ok(pb::BuiltinTitle::Rain) => Ok(BuiltinTitle::Rain),
        Ok(pb::BuiltinTitle::Storm) => Ok(BuiltinTitle::Storm),
        Ok(pb::BuiltinTitle::Wind) => Ok(BuiltinTitle::Wind),
        Ok(pb::BuiltinTitle::Waves) => Ok(BuiltinTitle::Waves),
        Ok(pb::BuiltinTitle::Stream) => Ok(BuiltinTitle::Stream),
        Ok(pb::BuiltinTitle::Birds) => Ok(BuiltinTitle::Birds),
        Ok(pb::BuiltinTitle::SummerNight) => Ok(BuiltinTitle::SummerNight),
        Ok(pb::BuiltinTitle::Fireplace) => Ok(BuiltinTitle::Fireplace),
        _ => Err(InvalidMessage {
            field: "builtin_title",
        }),
    }
}

fn builtin_title_message(value: BuiltinTitle) -> i32 {
    let value = match value {
        BuiltinTitle::Effects => pb::BuiltinTitle::Effects,
        BuiltinTitle::Imported => pb::BuiltinTitle::Imported,
        BuiltinTitle::Rain => pb::BuiltinTitle::Rain,
        BuiltinTitle::Storm => pb::BuiltinTitle::Storm,
        BuiltinTitle::Wind => pb::BuiltinTitle::Wind,
        BuiltinTitle::Waves => pb::BuiltinTitle::Waves,
        BuiltinTitle::Stream => pb::BuiltinTitle::Stream,
        BuiltinTitle::Birds => pb::BuiltinTitle::Birds,
        BuiltinTitle::SummerNight => pb::BuiltinTitle::SummerNight,
        BuiltinTitle::Fireplace => pb::BuiltinTitle::Fireplace,
    };
    value.into()
}

fn collection_kind(kind: i32) -> Result<CollectionKind, &'static str> {
    match pb::CollectionKind::try_from(kind) {
        Ok(pb::CollectionKind::Playlist) => Ok(CollectionKind::Playlist),
        Ok(pb::CollectionKind::Soundboard) => Ok(CollectionKind::Soundboard),
        Ok(pb::CollectionKind::Unspecified) | Err(_) => Err("must be a playlist or soundboard"),
    }
}

fn collection_kind_message(kind: CollectionKind) -> pb::CollectionKind {
    match kind {
        CollectionKind::Playlist => pb::CollectionKind::Playlist,
        CollectionKind::Soundboard => pb::CollectionKind::Soundboard,
    }
}

fn background_value(message: pb::Background) -> Result<Background, &'static str> {
    match message.kind.ok_or("kind is required")? {
        pb::background::Kind::Gradient(_) => Ok(Background::Gradient),
        pb::background::Kind::ImagePath(path) => ImageFilePath::try_new(PathBuf::from(path))
            .map(Background::Image)
            .map_err(|_| "image_path must be absolute"),
        pb::background::Kind::Preset(preset) => background_preset(preset).map(Background::Preset),
    }
}

fn background_message(background: &Background) -> pb::Background {
    let kind = match background {
        Background::Gradient => pb::background::Kind::Gradient(pb::GradientBackground {}),
        Background::Preset(preset) => {
            pb::background::Kind::Preset(background_preset_message(*preset).into())
        }
        Background::Image(path) => pb::background::Kind::ImagePath(path_text(path.as_ref())),
    };
    pb::Background { kind: Some(kind) }
}

fn background_preset(preset: i32) -> Result<BackgroundPreset, &'static str> {
    Ok(match pb::BackgroundPreset::try_from(preset) {
        Ok(pb::BackgroundPreset::Autumn) => BackgroundPreset::Autumn,
        Ok(pb::BackgroundPreset::Battle) => BackgroundPreset::Battle,
        Ok(pb::BackgroundPreset::Castle) => BackgroundPreset::Castle,
        Ok(pb::BackgroundPreset::Cathedral) => BackgroundPreset::Cathedral,
        Ok(pb::BackgroundPreset::Coast) => BackgroundPreset::Coast,
        Ok(pb::BackgroundPreset::Desert) => BackgroundPreset::Desert,
        Ok(pb::BackgroundPreset::Dragon) => BackgroundPreset::Dragon,
        Ok(pb::BackgroundPreset::Fire) => BackgroundPreset::Fire,
        Ok(pb::BackgroundPreset::Forest) => BackgroundPreset::Forest,
        Ok(pb::BackgroundPreset::Haunted) => BackgroundPreset::Haunted,
        Ok(pb::BackgroundPreset::Mountain) => BackgroundPreset::Mountain,
        Ok(pb::BackgroundPreset::Night) => BackgroundPreset::Night,
        Ok(pb::BackgroundPreset::Sky) => BackgroundPreset::Sky,
        Ok(pb::BackgroundPreset::Skulls) => BackgroundPreset::Skulls,
        Ok(pb::BackgroundPreset::Storm) => BackgroundPreset::Storm,
        Ok(pb::BackgroundPreset::Tavern) => BackgroundPreset::Tavern,
        Ok(pb::BackgroundPreset::Town) => BackgroundPreset::Town,
        Ok(pb::BackgroundPreset::Underworld) => BackgroundPreset::Underworld,
        Ok(pb::BackgroundPreset::Volcano) => BackgroundPreset::Volcano,
        Ok(pb::BackgroundPreset::Winter) => BackgroundPreset::Winter,
        Ok(pb::BackgroundPreset::Unspecified) | Err(_) => {
            return Err("preset must be one Tufo ships");
        }
    })
}

fn background_preset_message(preset: BackgroundPreset) -> pb::BackgroundPreset {
    match preset {
        BackgroundPreset::Autumn => pb::BackgroundPreset::Autumn,
        BackgroundPreset::Battle => pb::BackgroundPreset::Battle,
        BackgroundPreset::Castle => pb::BackgroundPreset::Castle,
        BackgroundPreset::Cathedral => pb::BackgroundPreset::Cathedral,
        BackgroundPreset::Coast => pb::BackgroundPreset::Coast,
        BackgroundPreset::Desert => pb::BackgroundPreset::Desert,
        BackgroundPreset::Dragon => pb::BackgroundPreset::Dragon,
        BackgroundPreset::Fire => pb::BackgroundPreset::Fire,
        BackgroundPreset::Forest => pb::BackgroundPreset::Forest,
        BackgroundPreset::Haunted => pb::BackgroundPreset::Haunted,
        BackgroundPreset::Mountain => pb::BackgroundPreset::Mountain,
        BackgroundPreset::Night => pb::BackgroundPreset::Night,
        BackgroundPreset::Sky => pb::BackgroundPreset::Sky,
        BackgroundPreset::Skulls => pb::BackgroundPreset::Skulls,
        BackgroundPreset::Storm => pb::BackgroundPreset::Storm,
        BackgroundPreset::Tavern => pb::BackgroundPreset::Tavern,
        BackgroundPreset::Town => pb::BackgroundPreset::Town,
        BackgroundPreset::Underworld => pb::BackgroundPreset::Underworld,
        BackgroundPreset::Volcano => pb::BackgroundPreset::Volcano,
        BackgroundPreset::Winter => pb::BackgroundPreset::Winter,
    }
}

fn entry_order(message: pb::EntryOrder) -> Result<EntryOrder, &'static str> {
    let key = match pb::EntryOrderKey::try_from(message.key) {
        Ok(pb::EntryOrderKey::Title) => EntryOrderKey::Title,
        Ok(pb::EntryOrderKey::Duration) => EntryOrderKey::Duration,
        Ok(pb::EntryOrderKey::DateAdded) => EntryOrderKey::DateAdded,
        Ok(pb::EntryOrderKey::Unspecified) | Err(_) => {
            return Err("key must be title, duration, or date added");
        }
    };
    Ok(EntryOrder {
        key,
        descending: message.descending,
    })
}

fn entry_order_message(order: EntryOrder) -> pb::EntryOrder {
    let key = match order.key {
        EntryOrderKey::Title => pb::EntryOrderKey::Title,
        EntryOrderKey::Duration => pb::EntryOrderKey::Duration,
        EntryOrderKey::DateAdded => pb::EntryOrderKey::DateAdded,
    };
    pb::EntryOrder {
        key: key.into(),
        descending: order.descending,
    }
}

fn sound_settings(message: pb::SoundSettings) -> Result<SoundSettings, &'static str> {
    let fade = |fade: Option<prost_types::Duration>| {
        fade.and_then(duration_value)
            .and_then(|duration| FadeDuration::try_new(duration).ok())
            .ok_or("fades must be between 0 and 60 s")
    };
    Ok(SoundSettings {
        looping: message.looping,
        volume: Volume::try_new(message.volume).map_err(|_| "volume must be between 0 and 1")?,
        fade_in: fade(message.fade_in)?,
        fade_out: fade(message.fade_out)?,
    })
}

fn sound_settings_message(sound: SoundSettings) -> pb::SoundSettings {
    pb::SoundSettings {
        looping: sound.looping,
        volume: sound.volume.into_inner(),
        fade_in: Some(duration_message(sound.fade_in.into_inner())),
        fade_out: Some(duration_message(sound.fade_out.into_inner())),
    }
}

/// Maps [`SoundIcon`] to and from `pb::SoundIcon`, which share variant names.
macro_rules! sound_icon_mappings {
    ($($variant:ident),+ $(,)?) => {
        fn sound_icon(icon: i32) -> Result<SoundIcon, &'static str> {
            match pb::SoundIcon::try_from(icon) {
                $(Ok(pb::SoundIcon::$variant) => Ok(SoundIcon::$variant),)+
                Ok(pb::SoundIcon::Unspecified) | Err(_) => Err("icon must be one Tufo ships"),
            }
        }

        fn sound_icon_message(icon: SoundIcon) -> pb::SoundIcon {
            match icon {
                $(SoundIcon::$variant => pb::SoundIcon::$variant,)+
            }
        }
    };
}

sound_icon_mappings!(
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

fn collection_id(text: &str) -> Result<CollectionId, Status> {
    parse_uuid(text)
        .map(CollectionId::from_uuid)
        .ok_or_else(|| invalid("collection_id", "must be a canonical UUID"))
}

fn collection_entry_id(text: &str) -> Result<CollectionEntryId, Status> {
    parse_uuid(text)
        .map(CollectionEntryId::from_uuid)
        .ok_or_else(|| invalid("collection_entry_id", "must be a canonical UUID"))
}

fn local_paths(paths: Vec<String>) -> Result<Vec<LocalPath>, Status> {
    let required_paths = required("paths", (!paths.is_empty()).then_some(paths))?;
    required_paths
        .into_iter()
        .map(|path| {
            LocalPath::try_new(PathBuf::from(path))
                .map_err(|_| invalid("paths", "must be absolute"))
        })
        .collect()
}

/// A path's text; every path type here guarantees UTF-8.
fn path_text(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tonic::Code;

    use super::*;

    fn absolute(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    fn snapshot() -> LibrarySnapshot {
        let id = uuid::Uuid::from_u128(7);
        LibrarySnapshot {
            revision: 3,
            collections: vec![CollectionView {
                builtin_title: None,
                id: CollectionId::from_uuid(id),
                kind: CollectionKind::Soundboard,
                title: Title::try_new("Storm".to_owned()).unwrap(),
                background: Background::Image(
                    ImageFilePath::try_new(absolute("storm.png")).unwrap(),
                ),
                order: EntryOrder {
                    key: EntryOrderKey::DateAdded,
                    descending: true,
                },
                source: CollectionSource::Linked {
                    folder: FolderPath::try_new(absolute("sfx")).unwrap(),
                    missing: true,
                },
                entries: vec![EntryView {
                    builtin_title: None,
                    id: CollectionEntryId::from_uuid(id),
                    audio_file_id: AudioFileId::from_uuid(id),
                    title: Title::try_new("Thunder".to_owned()).unwrap(),
                    path: AudioFilePath::try_new(absolute("thunder.wav")).unwrap(),
                    duration: Duration::from_millis(1_500),
                    added_at: Timestamp::from_second(1_790_000_000).unwrap(),
                    missing: false,
                    sound: SoundSettings {
                        looping: true,
                        volume: Volume::try_new(0.5).unwrap(),
                        fade_in: FadeDuration::try_new(Duration::from_secs(2)).unwrap(),
                        fade_out: FadeDuration::default(),
                    },
                    icon: SoundIcon::CloudLightning,
                }],
            }],
        }
    }

    #[test]
    fn snapshots_survive_the_round_trip() {
        let snapshot = snapshot();

        assert_eq!(
            library_snapshot(watch_library_response(&snapshot)).unwrap(),
            snapshot
        );
    }

    #[test]
    fn kenku_imports_survive_the_round_trip() {
        let request = ImportKenkuLibrary {
            kenku_folder: Some(FolderPath::try_new(absolute("Kenku FM")).unwrap()),
        };
        let report = KenkuImportReport {
            collections_imported: 2,
            files_imported: 5,
            skipped_collections: vec![Title::try_new("Combat".to_owned()).unwrap()],
            unreadable_files: vec![AudioFilePath::try_new(absolute("gone.ogg")).unwrap()],
            unsupported_sources: vec!["https://example.com/radio.mp3".to_owned()],
            discord_bot_token_imported: true,
        };
        let relative_folder = import_kenku_library_request(pb::ImportKenkuLibraryRequest {
            kenku_folder_path: Some("Kenku FM".to_owned()),
        });

        assert_eq!(
            import_kenku_library_request(import_kenku_library_message(&request)).unwrap(),
            request
        );
        assert_eq!(
            kenku_import_report(Some(kenku_import_report_message(&report))).unwrap(),
            report
        );
        assert_eq!(relative_folder.unwrap_err().code(), Code::InvalidArgument);
    }

    #[test]
    fn absent_update_fields_leave_their_properties_alone() {
        let id = CollectionId::from_uuid(uuid::Uuid::from_u128(9));
        let update = UpdateCollection {
            order: Some(EntryOrder::default()),
            ..UpdateCollection::default()
        };

        let (parsed_id, parsed) =
            update_collection_request(update_collection_message(id, &update)).unwrap();

        assert_eq!(parsed_id, id);
        assert_eq!(parsed, update);
    }

    #[test]
    fn every_preset_survives_the_round_trip() {
        for preset in BackgroundPreset::ALL {
            let request = CreateCollection {
                kind: CollectionKind::Playlist,
                title: Title::try_new("Tavern".to_owned()).unwrap(),
                background: Background::Preset(preset),
            };

            assert_eq!(
                create_collection_request(create_collection_message(&request)).unwrap(),
                request
            );
        }
    }

    #[test]
    fn every_sound_icon_survives_the_round_trip() {
        let id = CollectionEntryId::from_uuid(uuid::Uuid::from_u128(9));
        for &icon in SoundIcon::ALL {
            let update = UpdateCollectionEntry {
                icon: Some(icon),
                ..UpdateCollectionEntry::default()
            };

            assert_eq!(
                update_collection_entry_request(update_collection_entry_message(id, &update))
                    .unwrap(),
                (id, update)
            );
        }
    }

    #[test]
    fn malformed_requests_are_invalid_arguments() {
        let not_a_uuid = delete_collection_request(&pb::DeleteCollectionRequest {
            collection_id: "tavern".to_owned(),
        });
        let no_kind = import_paths_request(pb::ImportPathsRequest {
            kind: 0,
            paths: vec![path_text(&absolute("rpg"))],
        });
        let loud = update_collection_entry_request(pb::UpdateCollectionEntryRequest {
            collection_entry_id: uuid::Uuid::from_u128(1).to_string(),
            title: None,
            sound: Some(pb::SoundSettings {
                volume: 1.5,
                ..sound_settings_message(SoundSettings::default())
            }),
            icon: None,
        });
        let unknown_icon = update_collection_entry_request(pb::UpdateCollectionEntryRequest {
            collection_entry_id: uuid::Uuid::from_u128(1).to_string(),
            title: None,
            sound: None,
            icon: Some(999),
        });

        let no_background = create_collection_request(pb::CreateCollectionRequest {
            kind: pb::CollectionKind::Playlist.into(),
            title: "Tavern".to_owned(),
            background: None,
        });
        let unknown_preset = create_collection_request(pb::CreateCollectionRequest {
            kind: pb::CollectionKind::Playlist.into(),
            title: "Tavern".to_owned(),
            background: Some(pb::Background {
                kind: Some(pb::background::Kind::Preset(999)),
            }),
        });

        assert_eq!(not_a_uuid.unwrap_err().code(), Code::InvalidArgument);
        assert_eq!(no_background.unwrap_err().code(), Code::InvalidArgument);
        assert_eq!(unknown_preset.unwrap_err().code(), Code::InvalidArgument);
        assert_eq!(no_kind.unwrap_err().code(), Code::InvalidArgument);
        assert_eq!(loud.unwrap_err().code(), Code::InvalidArgument);
        assert_eq!(unknown_icon.unwrap_err().code(), Code::InvalidArgument);
    }
}
