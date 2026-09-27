//! Kenku FM's `redux-persist` state.

use std::{collections::HashMap, time::Duration};

use serde::{Deserialize, de::DeserializeOwned};
use tufo_application::library::import_kenku_library::{
    KenkuBackground, KenkuCollection, KenkuItem, KenkuPlayerSettings, KenkuSource,
};
use tufo_models::{
    library::{CollectionKind, FADE_DURATION_MAX, FadeDuration, SoundSettings, Volume},
    path::ImageFilePath,
    playback::{AudioFilePath, queue::RepeatMode},
};

use super::file_urls::file_url_path;

const KENKU_FADE_MS_DEFAULT: f64 = 100.0;
/// Mark a file or web address rather than a picture's name.
const CUSTOM_BACKGROUND_PREFIXES: [&str; 2] = ["file", "http"];

/// Each value is JSON text of its own.
type PersistedRoot = HashMap<String, String>;

#[derive(Debug, thiserror::Error)]
#[error("Kenku FM's {part} is malformed")]
pub(super) struct MalformedStateError {
    part: &'static str,
    #[source]
    source: serde_json::Error,
}

pub(super) fn collections(persisted: &str) -> Result<Vec<KenkuCollection>, MalformedStateError> {
    let root = parse::<PersistedRoot>("player state", persisted)?;
    let mut collections = Vec::new();
    if let Some(text) = root.get("playlists") {
        let playlists = parse::<PlaylistsState>("playlists", text)?;
        collections.extend(ordered(playlists.playlists).map(|playlist| {
            KenkuCollection {
                kind: CollectionKind::Playlist,
                title: playlist.title,
                background: background(&playlist.background),
                items: playlist
                    .tracks
                    .iter()
                    .filter_map(|id| playlists.tracks.get(id))
                    .map(|track| KenkuItem {
                        title: track.title.clone(),
                        source: source(&track.url),
                        sound: SoundSettings::default(),
                    })
                    .collect(),
            }
        }));
    }
    if let Some(text) = root.get("soundboards") {
        let soundboards = parse::<SoundboardsState>("soundboards", text)?;
        collections.extend(ordered(soundboards.soundboards).map(|soundboard| {
            KenkuCollection {
                kind: CollectionKind::Soundboard,
                title: soundboard.title,
                background: background(&soundboard.background),
                items: soundboard
                    .sounds
                    .iter()
                    .filter_map(|id| soundboards.sounds.get(id))
                    .map(|sound| KenkuItem {
                        title: sound.title.clone(),
                        source: source(&sound.url),
                        sound: sound.clamped_settings(),
                    })
                    .collect(),
            }
        }));
    }
    Ok(collections)
}

/// Settings that don't parse stay absent.
pub(super) fn player_settings(persisted: &str) -> Result<KenkuPlayerSettings, MalformedStateError> {
    let root = parse::<PersistedRoot>("playback state", persisted)?;
    Ok(KenkuPlayerSettings {
        volume: setting(&root, "volume").and_then(|volume: f32| Volume::try_new(volume).ok()),
        muted: setting(&root, "muted"),
        shuffle: setting(&root, "shuffle"),
        repeat: setting(&root, "repeat").and_then(|repeat: String| match repeat.as_str() {
            "off" => Some(RepeatMode::Off),
            "playlist" => Some(RepeatMode::Playlist),
            "track" => Some(RepeatMode::Track),
            _ => None,
        }),
    })
}

pub(super) fn discord_bot_token(persisted: &str) -> Result<Option<String>, MalformedStateError> {
    let root = parse::<PersistedRoot>("settings", persisted)?;
    let Some(text) = root.get("settings") else {
        return Ok(None);
    };
    let settings = parse::<Settings>("settings", text)?;
    Ok(settings
        .discord_token
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty()))
}

fn setting<T: DeserializeOwned>(root: &PersistedRoot, key: &str) -> Option<T> {
    serde_json::from_str(root.get(key)?).ok()
}

fn parse<T: DeserializeOwned>(part: &'static str, text: &str) -> Result<T, MalformedStateError> {
    serde_json::from_str(text).map_err(|source| MalformedStateError { part, source })
}

/// Rows by id, and the ids in display order.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Normalized<T> {
    by_id: HashMap<String, T>,
    all_ids: Vec<String>,
}

fn ordered<T>(mut normalized: Normalized<T>) -> impl Iterator<Item = T> {
    normalized
        .all_ids
        .into_iter()
        .filter_map(move |id| normalized.by_id.remove(&id))
}

#[derive(Deserialize)]
struct PlaylistsState {
    playlists: Normalized<Playlist>,
    tracks: HashMap<String, Track>,
}

#[derive(Deserialize)]
struct Playlist {
    #[serde(default)]
    title: String,
    #[serde(default)]
    background: String,
    #[serde(default)]
    tracks: Vec<String>,
}

#[derive(Deserialize)]
struct Track {
    #[serde(default)]
    title: String,
    url: String,
}

#[derive(Deserialize)]
struct SoundboardsState {
    soundboards: Normalized<Soundboard>,
    sounds: HashMap<String, Sound>,
}

#[derive(Deserialize)]
struct Soundboard {
    #[serde(default)]
    title: String,
    #[serde(default)]
    background: String,
    #[serde(default)]
    sounds: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Sound {
    #[serde(default)]
    title: String,
    url: String,
    #[serde(default, rename = "loop")]
    looping: bool,
    volume: Option<f32>,
    fade_in: Option<f64>,
    fade_out: Option<f64>,
}

impl Sound {
    fn clamped_settings(&self) -> SoundSettings {
        SoundSettings {
            looping: self.looping,
            volume: self
                .volume
                .filter(|volume| volume.is_finite())
                .and_then(|volume| Volume::try_new(volume.clamp(0.0, 1.0)).ok())
                .unwrap_or_default(),
            fade_in: fade(self.fade_in),
            fade_out: fade(self.fade_out),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    discord_token: Option<String>,
}

fn fade(milliseconds: Option<f64>) -> FadeDuration {
    let milliseconds = milliseconds
        .filter(|milliseconds| milliseconds.is_finite())
        .unwrap_or(KENKU_FADE_MS_DEFAULT)
        .clamp(0.0, FADE_DURATION_MAX.as_secs_f64() * 1000.0);
    FadeDuration::try_new(Duration::from_secs_f64(milliseconds / 1000.0)).unwrap_or_default()
}

fn source(url: &str) -> KenkuSource {
    file_url_path(url)
        .and_then(|path| AudioFilePath::try_new(path).ok())
        .map_or_else(
            || KenkuSource::Unsupported(url.to_owned()),
            KenkuSource::File,
        )
}

fn background(background: &str) -> KenkuBackground {
    if !CUSTOM_BACKGROUND_PREFIXES
        .iter()
        .any(|prefix| background.starts_with(prefix))
    {
        return KenkuBackground::Named(background.to_owned());
    }
    file_url_path(background)
        .and_then(|path| ImageFilePath::try_new(path).ok())
        .map_or(KenkuBackground::Unsupported, KenkuBackground::Image)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn persisted(root: &serde_json::Value) -> String {
        let encoded = root
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), json!(value.to_string())))
            .collect::<serde_json::Map<_, _>>();
        serde_json::Value::Object(encoded).to_string()
    }

    fn absolute_url(name: &str) -> (String, std::path::PathBuf) {
        let path = std::env::temp_dir().join(name);
        let text = path
            .to_str()
            .unwrap()
            .replace('\\', "/")
            .replace(' ', "%20");
        (format!("file://{text}"), path)
    }

    #[test]
    fn playlists_and_soundboards_come_in_kenku_order() {
        let (door_url, door_path) = absolute_url("door creak.wav");
        let player = persisted(&json!({
            "playlists": {
                "playlists": {
                    "byId": {
                        "b": {"id": "b", "title": "Boss", "background": "war", "tracks": ["t2", "gone", "t1"]},
                        "a": {"id": "a", "title": "Ambient", "background": "https://example.com/a.jpg", "tracks": []}
                    },
                    "allIds": ["b", "a"]
                },
                "tracks": {
                    "t1": {"id": "t1", "title": "One", "url": "https://example.com/one.mp3"},
                    "t2": {"id": "t2", "title": "Two", "url": door_url}
                }
            },
            "soundboards": {
                "soundboards": {
                    "byId": {"s": {"id": "s", "title": "Doors", "background": "sky", "sounds": ["d"]}},
                    "allIds": ["s", "missing"]
                },
                "sounds": {
                    "d": {"id": "d", "title": "Door", "url": door_url, "loop": true, "volume": 1.5, "fadeIn": 250, "fadeOut": 90000}
                }
            },
            "_persist": {"version": 1, "rehydrated": true}
        }));

        let collections = collections(&player).unwrap();

        let titles = collections
            .iter()
            .map(|collection| (collection.kind, collection.title.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            [
                (CollectionKind::Playlist, "Boss"),
                (CollectionKind::Playlist, "Ambient"),
                (CollectionKind::Soundboard, "Doors"),
            ]
        );
        assert_eq!(
            collections[0].background,
            KenkuBackground::Named("war".to_owned())
        );
        assert_eq!(collections[1].background, KenkuBackground::Unsupported);
        let door = KenkuSource::File(AudioFilePath::try_new(door_path).unwrap());
        assert_eq!(collections[0].items[0].source, door);
        assert_eq!(
            collections[0].items[1].source,
            KenkuSource::Unsupported("https://example.com/one.mp3".to_owned())
        );
        let sound = collections[2].items[0].sound;
        assert!(sound.looping);
        assert_eq!(sound.volume, Volume::try_new(1.0).unwrap());
        assert_eq!(sound.fade_in.into_inner(), Duration::from_millis(250));
        assert_eq!(sound.fade_out.into_inner(), FADE_DURATION_MAX);
    }

    #[test]
    fn a_custom_background_file_becomes_the_collections_image() {
        let (url, path) = absolute_url("map.png");

        assert_eq!(
            background(&url),
            KenkuBackground::Image(ImageFilePath::try_new(path).unwrap())
        );
    }

    #[test]
    fn saved_music_settings_parse_one_by_one() {
        let playback = persisted(&json!({
            "volume": 0.82,
            "muted": false,
            "shuffle": "not a bool",
            "repeat": "track",
            "_persist": {"version": 1, "rehydrated": true}
        }));

        let settings = player_settings(&playback).unwrap();

        assert_eq!(
            settings,
            KenkuPlayerSettings {
                volume: Some(Volume::try_new(0.82).unwrap()),
                muted: Some(false),
                shuffle: None,
                repeat: Some(RepeatMode::Track),
            }
        );
    }

    #[test]
    fn the_bot_token_comes_from_the_settings_when_present() {
        let with_token = persisted(&json!({
            "settings": {"discordToken": " abc.def ", "remotePort": "3333"},
            "bookmarks": {"bookmarks": {"byId": {}, "allIds": []}}
        }));
        let blank = persisted(&json!({"settings": {"discordToken": ""}}));

        assert_eq!(
            discord_bot_token(&with_token).unwrap().as_deref(),
            Some("abc.def")
        );
        assert_eq!(discord_bot_token(&blank).unwrap(), None);
        assert!(discord_bot_token("[]").is_err());
    }
}
