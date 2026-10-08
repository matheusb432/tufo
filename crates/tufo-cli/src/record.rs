use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use tufo_client::library::{
    Background, CollectionSource, CollectionView, EntryView, LibraryFolderView, LibrarySnapshot,
    SongSource, SongView,
};
use tufo_models::library::{BuiltinTitle, CollectionKind, EntryOrderKey, SoundSettings};

use crate::library_values::{background_preset, sound_icon};

mod imports;
mod player;
mod song_loops;

pub(crate) use imports::{import_report, kenku_status};
pub(crate) use player::{
    browser, diagnostics, music_layers, music_state, output, player, player_settings,
    queued_entry_ids, sounds, voice, voice_channel,
};
pub(crate) use song_loops::{loop_region, saved_loop, waveform};

pub(crate) fn library(snapshot: &LibrarySnapshot) -> Value {
    json!({
        "revision": snapshot.revision,
        "collections": snapshot.collections.iter().map(collection).collect::<Vec<_>>(),
        "songs": snapshot.songs.iter().map(song).collect::<Vec<_>>(),
        "library_folders": snapshot.library_folders.iter().map(folder).collect::<Vec<_>>(),
    })
}

pub(crate) fn collection(value: &CollectionView) -> Value {
    json!({
        "id": value.id.to_string(),
        "kind": collection_kind(value.kind),
        "title": value.title.as_str(),
        "builtin_title": value.builtin_title.map(builtin_title),
        "background": background(&value.background),
        "order": {
            "key": order_key(value.order.key),
            "descending": value.order.descending,
        },
        "source": collection_source(&value.source),
        "entries": value.entries.iter().map(entry).collect::<Vec<_>>(),
    })
}

pub(crate) fn entry(value: &EntryView) -> Value {
    json!({
        "id": value.id.to_string(),
        "audio_file_id": value.audio_file_id.to_string(),
        "title": value.title.as_str(),
        "builtin_title": value.builtin_title.map(builtin_title),
        "path": value.path.as_ref().display().to_string(),
        "duration_ms": duration_ms(value.duration),
        "added_at_unix_ms": value.added_at.as_millisecond(),
        "missing": value.missing,
        "sound": sound_settings(value.sound),
        "icon": sound_icon(value.icon),
        "saved_loop": value.saved_loop.as_ref().map(saved_loop),
    })
}

pub(crate) fn song(value: &SongView) -> Value {
    json!({
        "audio_file_id": value.audio_file_id.to_string(),
        "title": value.title.as_str(),
        "builtin_title": value.builtin_title.map(builtin_title),
        "path": value.path.as_ref().display().to_string(),
        "duration_ms": duration_ms(value.duration),
        "added_at_unix_ms": value.added_at.as_millisecond(),
        "missing": value.missing,
        "source": song_source(value.source),
        "saved_loop": value.saved_loop.as_ref().map(saved_loop),
    })
}

pub(crate) fn folder(value: &LibraryFolderView) -> Value {
    json!({
        "id": value.id.to_string(),
        "kind": collection_kind(value.kind),
        "folder": value.folder.as_ref().display().to_string(),
    })
}

fn collection_kind(value: CollectionKind) -> &'static str {
    match value {
        CollectionKind::Playlist => "playlist",
        CollectionKind::Soundboard => "soundboard",
    }
}

fn background(value: &Background) -> Value {
    match value {
        Background::Gradient => json!({"kind": "gradient"}),
        Background::Preset(preset) => {
            json!({"kind": "preset", "preset": background_preset(*preset)})
        }
        Background::Image(path) => {
            json!({"kind": "image", "path": path.as_ref().display().to_string()})
        }
    }
}

fn collection_source(value: &CollectionSource) -> Value {
    match value {
        CollectionSource::Manual => json!({"kind": "manual"}),
        CollectionSource::Linked { folder, missing } => json!({
            "kind": "linked",
            "folder": folder.as_ref().display().to_string(),
            "missing": missing,
        }),
    }
}

fn song_source(value: SongSource) -> Value {
    match value {
        SongSource::File => json!({"kind": "file"}),
        SongSource::Folder(id) => json!({"kind": "folder", "library_folder_id": id.to_string()}),
    }
}

fn order_key(value: EntryOrderKey) -> &'static str {
    match value {
        EntryOrderKey::Title => "title",
        EntryOrderKey::Duration => "duration",
        EntryOrderKey::DateAdded => "date_added",
    }
}

fn sound_settings(value: SoundSettings) -> Value {
    json!({
        "looping": value.looping,
        "volume": value.volume.into_inner(),
        "fade_in_ms": duration_ms(value.fade_in.into_inner()),
        "fade_out_ms": duration_ms(value.fade_out.into_inner()),
    })
}

fn builtin_title(value: BuiltinTitle) -> &'static str {
    match value {
        BuiltinTitle::Effects => "effects",
        BuiltinTitle::Imported => "imported",
        BuiltinTitle::Rain => "rain",
        BuiltinTitle::Storm => "storm",
        BuiltinTitle::Wind => "wind",
        BuiltinTitle::Waves => "waves",
        BuiltinTitle::Stream => "stream",
        BuiltinTitle::Birds => "birds",
        BuiltinTitle::SummerNight => "summer_night",
        BuiltinTitle::Fireplace => "fireplace",
    }
}

fn duration_ms(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}

fn timestamp_unix_ms(value: SystemTime) -> i64 {
    match value.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
        Err(error) => {
            let milliseconds = error.duration().as_nanos().div_ceil(1_000_000);
            i64::try_from(milliseconds).map_or(i64::MIN, |value| -value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_down_on_both_sides_of_the_unix_epoch() {
        assert_eq!(
            timestamp_unix_ms(SystemTime::UNIX_EPOCH + Duration::from_micros(1_001)),
            1
        );
        assert_eq!(
            timestamp_unix_ms(SystemTime::UNIX_EPOCH - Duration::from_micros(1)),
            -1
        );
        assert_eq!(
            timestamp_unix_ms(SystemTime::UNIX_EPOCH - Duration::from_micros(1_001)),
            -2
        );
    }
}
