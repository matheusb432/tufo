//! Deciding what an import creates, from data alone.

use std::collections::{HashMap, HashSet};

use tufo_models::{
    library::{BackgroundPreset, BackgroundPresetChooser, CollectionKind, SoundSettings, Title},
    path::FolderPath,
    playback::AudioFilePath,
};
use tufo_wire::library::Background;

use super::{
    kenku_library::{KenkuBackground, KenkuCollection, KenkuSource},
    kenku_titles::kenku_default_title,
};

/// Kenku FM's abstract pictures are left out and get a preset like new collections do.
const KENKU_BACKGROUND_PRESETS: [(&str, BackgroundPreset); 18] = [
    ("autumn", BackgroundPreset::Autumn),
    ("beach", BackgroundPreset::Coast),
    ("castle", BackgroundPreset::Castle),
    ("cathedral", BackgroundPreset::Cathedral),
    ("dragon", BackgroundPreset::Dragon),
    ("haunted", BackgroundPreset::Haunted),
    ("mountain", BackgroundPreset::Mountain),
    ("sand", BackgroundPreset::Desert),
    ("skulls", BackgroundPreset::Skulls),
    ("sky", BackgroundPreset::Sky),
    ("sparks", BackgroundPreset::Fire),
    ("stars", BackgroundPreset::Night),
    ("tavern", BackgroundPreset::Tavern),
    ("town", BackgroundPreset::Town),
    ("tree", BackgroundPreset::Forest),
    ("volcano", BackgroundPreset::Volcano),
    ("war", BackgroundPreset::Battle),
    ("winter", BackgroundPreset::Winter),
];

pub(super) struct PlanInput {
    pub(super) collections: Vec<KenkuCollection>,
    /// Shown collections' kinds and titles.
    pub(super) existing: HashSet<(CollectionKind, Title)>,
    pub(super) shown_presets: Vec<BackgroundPreset>,
    /// Each completely scanned candidate's direct audio files.
    pub(super) folder_audio_files: HashMap<FolderPath, Vec<AudioFilePath>>,
    pub(super) rng: fastrand::Rng,
}

#[derive(Debug, PartialEq)]
pub(super) struct ImportPlan {
    pub(super) collections: Vec<PlannedCollection>,
    /// Titles the library already shows.
    pub(super) skipped: Vec<Title>,
    /// Web addresses of the imported tracks and sounds.
    pub(super) unsupported_sources: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub(super) struct PlannedCollection {
    pub(super) kind: CollectionKind,
    pub(super) title: Title,
    pub(super) background: Background,
    /// `None` for a manual collection.
    pub(super) folder: Option<FolderPath>,
    pub(super) files: Vec<PlannedFile>,
}

#[derive(Debug, PartialEq)]
pub(super) struct PlannedFile {
    pub(super) path: AudioFilePath,
    /// Set when someone typed the title in Kenku FM.
    pub(super) typed_title: Option<Title>,
    pub(super) sound: SoundSettings,
}

/// The one folder holding all of a collection's files.
pub(super) fn folder_candidate(collection: &KenkuCollection) -> Option<FolderPath> {
    let mut folders = collection.items.iter().map(|item| match &item.source {
        KenkuSource::File(path) => path.as_ref().parent(),
        KenkuSource::Unsupported(_) => None,
    });
    let first = folders.next()??;
    if !folders.all(|folder| folder == Some(first)) {
        return None;
    }
    FolderPath::try_new(first.to_path_buf()).ok()
}

pub(super) fn plan(input: PlanInput) -> ImportPlan {
    let PlanInput {
        collections,
        existing,
        shown_presets,
        folder_audio_files,
        rng,
    } = input;
    let (skipped, imported): (Vec<_>, Vec<_>) = collections
        .into_iter()
        .map(|collection| (Title::from_name_lossy(&collection.title), collection))
        .partition(|(title, collection)| existing.contains(&(collection.kind, title.clone())));
    let named_presets = imported
        .iter()
        .filter_map(|(_, collection)| named_preset(&collection.background));
    let mut presets =
        BackgroundPresetChooser::new(shown_presets.into_iter().chain(named_presets), rng);
    let mut followed = HashSet::new();
    let mut plan = ImportPlan {
        collections: Vec::with_capacity(imported.len()),
        skipped: skipped.into_iter().map(|(title, _)| title).collect(),
        unsupported_sources: Vec::new(),
    };
    for (title, collection) in imported {
        let background = match &collection.background {
            KenkuBackground::Image(image) => Background::Image(image.clone()),
            other => Background::Preset(named_preset(other).unwrap_or_else(|| presets.choose())),
        };
        let folder = folder_candidate(&collection).filter(|folder| {
            holds_exactly(folder_audio_files.get(folder), &collection)
                && followed.insert((collection.kind, folder.clone()))
        });
        let mut files = Vec::with_capacity(collection.items.len());
        for item in collection.items {
            match item.source {
                KenkuSource::File(path) => files.push(PlannedFile {
                    typed_title: typed_title(&item.title, &path),
                    path,
                    sound: item.sound,
                }),
                KenkuSource::Unsupported(address) => plan.unsupported_sources.push(address),
            }
        }
        plan.collections.push(PlannedCollection {
            kind: collection.kind,
            title,
            background,
            folder,
            files,
        });
    }
    plan
}

fn named_preset(background: &KenkuBackground) -> Option<BackgroundPreset> {
    let KenkuBackground::Named(name) = background else {
        return None;
    };
    KENKU_BACKGROUND_PRESETS
        .iter()
        .find(|(kenku_name, _)| kenku_name.eq_ignore_ascii_case(name))
        .map(|(_, preset)| *preset)
}

fn holds_exactly(folder_files: Option<&Vec<AudioFilePath>>, collection: &KenkuCollection) -> bool {
    let Some(folder_files) = folder_files.filter(|files| !files.is_empty()) else {
        return false;
    };
    let in_folder = folder_files
        .iter()
        .map(AsRef::<std::path::PathBuf>::as_ref)
        .collect::<HashSet<_>>();
    let in_collection = collection
        .items
        .iter()
        .filter_map(|item| match &item.source {
            KenkuSource::File(path) => Some(path.as_ref()),
            KenkuSource::Unsupported(_) => None,
        })
        .collect::<HashSet<_>>();
    in_folder == in_collection
}

fn typed_title(kenku_title: &str, path: &AudioFilePath) -> Option<Title> {
    let file_name = path.as_ref().file_name().and_then(|name| name.to_str())?;
    if kenku_default_title(file_name) == kenku_title {
        return None;
    }
    Title::try_new(kenku_title.to_owned()).ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tufo_models::{
        library::{FadeDuration, Volume},
        path::ImageFilePath,
    };

    use super::*;
    use crate::library::import_kenku_library::kenku_library::KenkuItem;

    fn absolute(relative: &str) -> PathBuf {
        let root = if cfg!(windows) { r"C:\music" } else { "/music" };
        relative
            .split('/')
            .fold(PathBuf::from(root), |path, part| path.join(part))
    }

    fn audio_path(relative: &str) -> AudioFilePath {
        AudioFilePath::try_new(absolute(relative)).unwrap()
    }

    fn folder_path(relative: &str) -> FolderPath {
        FolderPath::try_new(absolute(relative)).unwrap()
    }

    fn title(text: &str) -> Title {
        Title::try_new(text.to_owned()).unwrap()
    }

    fn file_item(relative: &str) -> KenkuItem {
        let name = relative.rsplit('/').next().unwrap();
        KenkuItem {
            title: kenku_default_title(name),
            source: KenkuSource::File(audio_path(relative)),
            sound: SoundSettings::default(),
        }
    }

    fn collection(kind: CollectionKind, title: &str, items: Vec<KenkuItem>) -> KenkuCollection {
        KenkuCollection {
            kind,
            title: title.to_owned(),
            background: KenkuBackground::Named("sky".to_owned()),
            items,
        }
    }

    fn input(collections: Vec<KenkuCollection>) -> PlanInput {
        PlanInput {
            collections,
            existing: HashSet::new(),
            shown_presets: Vec::new(),
            folder_audio_files: HashMap::new(),
            rng: fastrand::Rng::with_seed(7),
        }
    }

    #[test]
    fn a_collection_of_one_folders_files_follows_that_folder() {
        let combat = collection(
            CollectionKind::Playlist,
            "combat",
            vec![file_item("combat/a.ogg"), file_item("combat/b.ogg")],
        );
        let mut input = input(vec![combat]);
        input.folder_audio_files.insert(
            folder_path("combat"),
            vec![audio_path("combat/b.ogg"), audio_path("combat/a.ogg")],
        );

        let plan = plan(input);

        assert_eq!(plan.collections[0].folder, Some(folder_path("combat")));
    }

    #[test]
    fn a_collection_missing_some_of_its_folders_files_stays_manual() {
        let combat = collection(
            CollectionKind::Playlist,
            "combat",
            vec![file_item("combat/a.ogg")],
        );
        let mut input = input(vec![combat]);
        input.folder_audio_files.insert(
            folder_path("combat"),
            vec![audio_path("combat/a.ogg"), audio_path("combat/b.ogg")],
        );

        assert_eq!(plan(input).collections[0].folder, None);
    }

    #[test]
    fn collections_spanning_folders_or_the_web_have_no_folder_candidate() {
        let spanning = collection(
            CollectionKind::Playlist,
            "mixed",
            vec![file_item("combat/a.ogg"), file_item("boss/b.ogg")],
        );
        let mut streamed = collection(
            CollectionKind::Playlist,
            "streamed",
            vec![file_item("combat/a.ogg")],
        );
        streamed.items.push(KenkuItem {
            title: "Radio".to_owned(),
            source: KenkuSource::Unsupported("https://example.com/radio.mp3".to_owned()),
            sound: SoundSettings::default(),
        });

        assert_eq!(folder_candidate(&spanning), None);
        assert_eq!(folder_candidate(&streamed), None);
        assert_eq!(
            folder_candidate(&collection(CollectionKind::Playlist, "empty", Vec::new())),
            None
        );
    }

    #[test]
    fn only_the_first_collection_of_a_kind_follows_a_folder() {
        let items = || vec![file_item("combat/a.ogg")];
        let mut input = input(vec![
            collection(CollectionKind::Playlist, "combat", items()),
            collection(CollectionKind::Playlist, "combat again", items()),
            collection(CollectionKind::Soundboard, "combat sounds", items()),
        ]);
        input
            .folder_audio_files
            .insert(folder_path("combat"), vec![audio_path("combat/a.ogg")]);

        let folders = plan(input)
            .collections
            .into_iter()
            .map(|planned| planned.folder)
            .collect::<Vec<_>>();

        assert_eq!(
            folders,
            [
                Some(folder_path("combat")),
                None,
                Some(folder_path("combat"))
            ]
        );
    }

    #[test]
    fn collections_the_library_shows_are_skipped_by_kind_and_title() {
        let mut input = input(vec![
            collection(CollectionKind::Playlist, " combat ", Vec::new()),
            collection(CollectionKind::Soundboard, "combat", Vec::new()),
        ]);
        input
            .existing
            .insert((CollectionKind::Playlist, title("combat")));

        let plan = plan(input);

        assert_eq!(plan.skipped, [title("combat")]);
        assert_eq!(plan.collections.len(), 1);
        assert_eq!(plan.collections[0].kind, CollectionKind::Soundboard);
    }

    #[test]
    fn kenku_pictures_become_their_presets_and_the_rest_get_unused_ones() {
        let mut war = collection(CollectionKind::Playlist, "war", Vec::new());
        war.background = KenkuBackground::Named("war".to_owned());
        let mut abstract_picture = collection(CollectionKind::Playlist, "veins", Vec::new());
        abstract_picture.background = KenkuBackground::Named("veins".to_owned());
        let mut web = collection(CollectionKind::Playlist, "web", Vec::new());
        web.background = KenkuBackground::Unsupported;
        let image = ImageFilePath::try_new(absolute("art/map.png")).unwrap();
        let mut own = collection(CollectionKind::Playlist, "own", Vec::new());
        own.background = KenkuBackground::Image(image.clone());
        let mut input = input(vec![war, abstract_picture, web, own]);
        input.shown_presets = BackgroundPreset::ALL
            .into_iter()
            .filter(|preset| !matches!(preset, BackgroundPreset::Storm | BackgroundPreset::Tavern))
            .collect();

        let backgrounds = plan(input)
            .collections
            .into_iter()
            .map(|planned| planned.background)
            .collect::<Vec<_>>();

        assert_eq!(backgrounds[0], Background::Preset(BackgroundPreset::Battle));
        let mut chosen = backgrounds[1..3]
            .iter()
            .filter_map(|background| match background {
                Background::Preset(preset) => Some(*preset),
                _ => None,
            })
            .collect::<Vec<_>>();
        chosen.sort();
        assert_eq!(chosen, [BackgroundPreset::Storm, BackgroundPreset::Tavern]);
        assert_eq!(backgrounds[3], Background::Image(image));
    }

    #[test]
    fn only_titles_typed_in_kenku_replace_tufos() {
        let mut renamed = file_item("tunes/[good]-victory-horns.mp3");
        renamed.title = "Victory Fanfare".to_owned();
        let tunes = collection(
            CollectionKind::Soundboard,
            "tunes",
            vec![file_item("tunes/[sad]-low-horn.MP3"), renamed],
        );

        let titles = plan(input(vec![tunes])).collections[0]
            .files
            .iter()
            .map(|file| file.typed_title.clone())
            .collect::<Vec<_>>();

        assert_eq!(titles, [None, Some(title("Victory Fanfare"))]);
    }

    #[test]
    fn web_tracks_are_reported_and_sound_settings_carry_over() {
        let sound = SoundSettings {
            looping: true,
            volume: Volume::try_new(0.25).unwrap(),
            fade_in: FadeDuration::try_new(std::time::Duration::from_millis(300)).unwrap(),
            fade_out: FadeDuration::default(),
        };
        let mut rain = file_item("sfx/rain.ogg");
        rain.sound = sound;
        let web = KenkuItem {
            title: "Radio".to_owned(),
            source: KenkuSource::Unsupported("https://example.com/radio.mp3".to_owned()),
            sound: SoundSettings::default(),
        };
        let board = collection(CollectionKind::Soundboard, "weather", vec![rain, web]);

        let plan = plan(input(vec![board]));

        assert_eq!(plan.unsupported_sources, ["https://example.com/radio.mp3"]);
        assert_eq!(plan.collections[0].files.len(), 1);
        assert_eq!(plan.collections[0].files[0].sound, sound);
    }
}
