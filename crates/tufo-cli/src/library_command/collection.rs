use std::{path::PathBuf, str::FromStr};

use clap::Subcommand;
use serde::Deserialize;
use serde_json::{Value, json};
use tufo_client::{
    import_resources,
    library::{
        AddFilesToCollection, AddSongsToPlaylist, Background, CreateCollection, EntryOrderPatch,
        UpdateCollection,
    },
};
use tufo_models::{
    library::{AudioFileId, CollectionId, CollectionKind, EntryOrderKey, Title},
    path::{ImageFilePath, LocalPath},
};

use super::{collection_by_id, matches_query, parse_collection_kind, parse_paths, resource_ids};
use crate::{
    api,
    input::{NullableField, deserialize_present, parse_audio_file_id, parse_collection_id},
    library_values::parse_background_preset,
    record,
};

#[derive(Subcommand)]
pub(crate) enum CollectionCommand {
    /// List playlists and soundboards, including their entries and stable IDs.
    List {
        /// Limit results to playlist or soundboard.
        #[arg(long, value_parser = parse_collection_kind)]
        kind: Option<CollectionKind>,
    },
    /// Read a collection by its stable ID.
    Get {
        #[arg(value_name = "COLLECTION_ID", value_parser = parse_collection_id)]
        id: CollectionId,
    },
    /// Find collections whose titles contain every query word, ignoring case.
    Search {
        query: String,
        /// Limit results to playlist or soundboard.
        #[arg(long, value_parser = parse_collection_kind)]
        kind: Option<CollectionKind>,
    },
    /// Create a manual playlist or soundboard.
    #[command(
        visible_alias = "create",
        after_long_help = r#"Examples:
  tufo collection add '{"kind":"playlist","title":"Travel"}'
  tufo collection add '{"kind":"soundboard","title":"Weather","background":{"kind":"preset","preset":"storm"}}'

Required fields: kind (playlist or soundboard), title.
Omitting background keeps the domain default gradient.
Backgrounds: {"kind":"gradient"}, {"kind":"preset","preset":"forest"},
or {"kind":"image","path":"/srv/art/forest.webp"}. Images require absolute paths.
Presets: autumn, battle, castle, cathedral, coast, desert, dragon, fire, forest,
haunted, mountain, night, sky, skulls, storm, tavern, town, underworld, volcano, winter."#
    )]
    Add {
        #[arg(value_name = "JSON_PAYLOAD")]
        payload: AddPayload,
    },
    /// Change title, background, entry ordering, or pinning; omitted fields stay as they are.
    #[command(after_long_help = r#"Examples (use an ID from collection list):
  tufo collection update COLLECTION_ID '{"title":"Night travel","order":{"key":"duration","descending":true}}'
  tufo collection update COLLECTION_ID '{"background":null}'
  tufo collection update COLLECTION_ID '{"pinned":false}'

Order keys: title, duration, date_added. Omitted order properties are preserved.
background accepts the same objects as collection add; null restores the gradient.
pinned lists the collection in the GUI's sidebar; new collections start pinned.
Other fields cannot be null. Unknown fields, including nested fields, are rejected.
Nested updates require matching current binaries; after updating them, run tufo server restart."#)]
    Update {
        #[arg(value_name = "COLLECTION_ID", value_parser = parse_collection_id)]
        id: CollectionId,
        #[arg(value_name = "JSON_PAYLOAD")]
        payload: UpdatePayload,
    },
    /// Remove a collection from Tufo.
    Delete {
        #[arg(value_name = "COLLECTION_ID", value_parser = parse_collection_id)]
        id: CollectionId,
    },
    /// Add audio paths to a manual collection, returning counts and matching resource IDs.
    #[command(after_long_help = r#"Example (use an ID from collection list):
  tufo collection add-files COLLECTION_ID '{"paths":["/srv/audio/rain.wav"]}'

paths must contain at least one absolute file or folder path."#)]
    AddFiles {
        #[arg(value_name = "COLLECTION_ID", value_parser = parse_collection_id)]
        id: CollectionId,
        #[arg(value_name = "JSON_PAYLOAD")]
        payload: AddFilesPayload,
    },
    /// Add existing songs to a manual playlist, returning matching entry IDs.
    #[command(
        after_long_help = r#"Example (use IDs from collection list and song list):
  tufo collection add-songs COLLECTION_ID '{"audio_file_ids":["01a0cf81-04dd-7d42-9a53-2904e3318bc7"]}'

audio_file_ids must contain at least one lowercase hyphenated UUID."#
    )]
    AddSongs {
        #[arg(value_name = "COLLECTION_ID", value_parser = parse_collection_id)]
        id: CollectionId,
        #[arg(value_name = "JSON_PAYLOAD")]
        payload: AddSongsPayload,
    },
}

pub(crate) async fn run_collection(command: CollectionCommand, json: bool) -> anyhow::Result<bool> {
    let client = api::connect().await?;
    match command {
        CollectionCommand::List { kind } => {
            let snapshot = api::library_snapshot(&client).await?;
            let records = snapshot
                .collections
                .iter()
                .filter(|collection| kind.is_none_or(|kind| collection.kind == kind))
                .map(record::collection)
                .collect::<Vec<_>>();
            api::emit(&Value::Array(records), json).await?;
        }
        CollectionCommand::Get { id } => {
            let snapshot = api::library_snapshot(&client).await?;
            api::emit(&record::collection(collection_by_id(&snapshot, id)?), json).await?;
        }
        CollectionCommand::Search { query, kind } => {
            let snapshot = api::library_snapshot(&client).await?;
            let records = snapshot
                .collections
                .iter()
                .filter(|collection| kind.is_none_or(|kind| collection.kind == kind))
                .filter(|collection| matches_query(collection.title.as_str(), &query))
                .map(record::collection)
                .collect::<Vec<_>>();
            api::emit(&Value::Array(records), json).await?;
        }
        CollectionCommand::Add { payload } => {
            let id = client.create_collection(&payload.0).await?;
            api::ack("add", "collection", id, json).await?;
        }
        CollectionCommand::Update { id, payload } => {
            client.update_collection(id, &payload.0).await?;
            api::ack("update", "collection", id, json).await?;
        }
        CollectionCommand::Delete { id } => {
            client.delete_collection(id).await?;
            api::ack("delete", "collection", id, json).await?;
        }
        CollectionCommand::AddFiles { id, payload } => {
            let correlation = AddFilesToCollection {
                collection_id: id,
                paths: import_resources::resolve_paths(payload.0.clone()).await?,
            };
            let request = AddFilesToCollection {
                collection_id: id,
                paths: payload.0,
            };
            let report = client.add_files_to_collection(&request).await?;
            let after = api::library_snapshot(&client).await?;
            api::emit(
                &json!({
                    "action": "add-files", "resource": "collection", "id": id.to_string(),
                    "report": record::import_report(&report),
                    "resources": resource_ids(&import_resources::resources_for_collection_files(&after, &correlation)),
                    "revision": after.revision,
                }),
                json,
            ).await?;
        }
        CollectionCommand::AddSongs { id, payload } => {
            let request = AddSongsToPlaylist {
                playlist_id: id,
                audio_file_ids: payload.0,
            };
            let songs_added = client.add_songs_to_playlist(&request).await?;
            let after = api::library_snapshot(&client).await?;
            api::emit(
                &json!({
                    "action": "add-songs", "resource": "collection", "id": id.to_string(),
                    "songs_added": songs_added,
                    "resources": resource_ids(&import_resources::resources_for_playlist_songs(&after, &request)),
                    "revision": after.revision,
                }),
                json,
            )
            .await?;
        }
    }
    Ok(true)
}

#[derive(Debug, Clone)]
pub(crate) struct AddPayload(CreateCollection);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddJson {
    kind: String,
    title: String,
    #[serde(default, deserialize_with = "deserialize_present")]
    background: Option<BackgroundJson>,
}

impl FromStr for AddPayload {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let input: AddJson = crate::input::parse_json(text)?;
        Ok(Self(CreateCollection {
            kind: parse_collection_kind(&input.kind)?,
            title: Title::try_new(input.title)
                .map_err(|error| format!("invalid title: {error}"))?,
            background: input
                .background
                .map(Background::try_from)
                .transpose()?
                .unwrap_or_default(),
        }))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct UpdatePayload(UpdateCollection);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateJson {
    #[serde(default, deserialize_with = "deserialize_present")]
    title: Option<String>,
    #[serde(default)]
    background: NullableField<BackgroundJson>,
    #[serde(default, deserialize_with = "deserialize_present")]
    order: Option<OrderJson>,
    #[serde(default, deserialize_with = "deserialize_present")]
    pinned: Option<bool>,
}

impl FromStr for UpdatePayload {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let input: UpdateJson = crate::input::parse_json(text)?;
        Ok(Self(UpdateCollection {
            title: input
                .title
                .map(Title::try_new)
                .transpose()
                .map_err(|error| format!("invalid title: {error}"))?,
            background: match input.background {
                NullableField::Omitted => None,
                NullableField::Clear => Some(Background::default()),
                NullableField::Set(background) => Some(Background::try_from(background)?),
            },
            order_patch: input.order.map(EntryOrderPatch::try_from).transpose()?,
            pinned: input.pinned,
            ..UpdateCollection::default()
        }))
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum BackgroundJson {
    Gradient {},
    Preset { preset: String },
    Image { path: String },
}

impl TryFrom<BackgroundJson> for Background {
    type Error = String;

    fn try_from(input: BackgroundJson) -> Result<Self, Self::Error> {
        match input {
            BackgroundJson::Gradient {} => Ok(Self::Gradient),
            BackgroundJson::Preset { preset } => parse_background_preset(&preset).map(Self::Preset),
            BackgroundJson::Image { path } => ImageFilePath::try_new(PathBuf::from(path))
                .map(Self::Image)
                .map_err(|_| "background.path must be an absolute UTF-8 path".to_owned()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderJson {
    #[serde(default, deserialize_with = "deserialize_present")]
    key: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    descending: Option<bool>,
}

impl TryFrom<OrderJson> for EntryOrderPatch {
    type Error = String;

    fn try_from(input: OrderJson) -> Result<Self, Self::Error> {
        let key = input
            .key
            .map(|key| match key.as_str() {
                "title" => Ok(EntryOrderKey::Title),
                "duration" => Ok(EntryOrderKey::Duration),
                "date_added" => Ok(EntryOrderKey::DateAdded),
                _ => Err("order.key must be title, duration, or date_added".to_owned()),
            })
            .transpose()?;
        Ok(Self {
            key,
            descending: input.descending,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AddFilesPayload(Vec<LocalPath>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddFilesJson {
    paths: Vec<String>,
}

impl FromStr for AddFilesPayload {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let input: AddFilesJson = crate::input::parse_json(text)?;
        parse_paths(input.paths).map(Self)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AddSongsPayload(Vec<AudioFileId>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddSongsJson {
    audio_file_ids: Vec<String>,
}

impl FromStr for AddSongsPayload {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let input: AddSongsJson = crate::input::parse_json(text)?;
        if input.audio_file_ids.is_empty() {
            return Err("audio_file_ids must contain at least one song ID".to_owned());
        }
        input
            .audio_file_ids
            .iter()
            .map(|id| parse_audio_file_id(id))
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creation_keeps_domain_defaults_and_trims_the_title() -> Result<(), String> {
        let payload: AddPayload = r#"{"kind":"playlist","title":"  Travel  "}"#.parse()?;
        assert_eq!(payload.0.kind, CollectionKind::Playlist);
        assert_eq!(payload.0.title.as_str(), "Travel");
        assert_eq!(payload.0.background, Background::default());
        Ok(())
    }

    #[test]
    fn nested_backgrounds_reject_unknown_fields_and_invalid_paths() {
        for background in [
            r#"{"kind":"gradient","extra":true}"#,
            r#"{"kind":"preset","preset":"forest","path":"/srv/art"}"#,
            r#"{"kind":"image","path":"relative"}"#,
            r#"{"kind":"preset","preset":"unknown"}"#,
            "null",
        ] {
            let payload =
                format!(r#"{{"kind":"playlist","title":"Travel","background":{background}}}"#);
            assert!(payload.parse::<AddPayload>().is_err(), "{payload}");
        }
    }

    #[test]
    fn background_clearing_and_nested_order_updates_preserve_omissions() -> Result<(), String> {
        let omitted: UpdatePayload = "{}".parse()?;
        assert!(omitted.0.title.is_none());
        assert!(omitted.0.background.is_none());
        assert!(omitted.0.order.is_none() && omitted.0.order_patch.is_none());
        assert!(omitted.0.pinned.is_none());
        let payload: UpdatePayload =
            r#"{"background":null,"order":{"descending":false},"pinned":false}"#.parse()?;
        assert_eq!(payload.0.background, Some(Background::Gradient));
        assert_eq!(payload.0.pinned, Some(false));
        assert!(payload.0.order.is_none());
        let order = payload.0.order_patch.ok_or("order patch was omitted")?;
        assert_eq!(order.key, None);
        assert_eq!(order.descending, Some(false));
        Ok(())
    }

    #[test]
    fn updates_reject_invalid_fields_before_execution() {
        for payload in [
            r#"{"title":"  "}"#,
            r#"{"title":null}"#,
            r#"{"unknown":true}"#,
            r#"{"order":{"key":"path"}}"#,
            r#"{"order":{"descending":null}}"#,
            r#"{"order":{"extra":true}}"#,
            r#"{"order":null}"#,
            r#"{"pinned":null}"#,
            r#"{"pinned":"yes"}"#,
        ] {
            assert!(payload.parse::<UpdatePayload>().is_err(), "{payload}");
        }
        assert!(r#"{"audio_file_ids":[]}"#.parse::<AddSongsPayload>().is_err());
        assert!(r#"{"audio_file_ids":["invalid"]}"#.parse::<AddSongsPayload>().is_err());
    }
}
