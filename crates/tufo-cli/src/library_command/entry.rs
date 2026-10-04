use std::{str::FromStr, time::Duration};

use clap::Subcommand;
use serde::Deserialize;
use serde_json::{Value, json};
use tufo_client::library::{
    CollectionView, EntryView, LibrarySnapshot, SoundSettingsPatch, UpdateCollectionEntry,
};
use tufo_models::library::{
    CollectionEntryId, CollectionId, CollectionKind, FadeDuration, SoundIcon, Title, Volume,
};

use super::{collection_by_id, matches_query};
use crate::{
    api,
    input::{NullableField, deserialize_present, parse_collection_id, parse_entry_id},
    library_values::parse_icon,
    record,
};

#[derive(Subcommand)]
pub(crate) enum EntryCommand {
    /// List collection entries with their collection and audio file IDs.
    List {
        /// Limit results to this collection.
        #[arg(long = "collection", value_name = "COLLECTION_ID", value_parser = parse_collection_id)]
        collection_id: Option<CollectionId>,
    },
    /// Read an entry by its stable ID.
    Get {
        #[arg(value_name = "ENTRY_ID", value_parser = parse_entry_id)]
        id: CollectionEntryId,
    },
    /// Find entries by title, path, or collection title, ignoring case.
    Search {
        query: String,
        /// Limit results to this collection.
        #[arg(long = "collection", value_name = "COLLECTION_ID", value_parser = parse_collection_id)]
        collection_id: Option<CollectionId>,
    },
    /// Edit an entry's title, icon, or sound settings, preserving omitted fields.
    #[command(after_long_help = r#"Examples (use an ID from entry list):
  tufo entry update ENTRY_ID '{"title":"Rain","icon":"cloud_rain","sound":{"volume":0.4,"looping":true}}'
  tufo entry update ENTRY_ID '{"sound":{"fade_in_ms":500,"fade_out_ms":1000}}'
  tufo entry update ENTRY_ID '{"icon":null}'

Renaming changes the audio file's title everywhere it appears.
sound accepts looping, volume (0–1), fade_in_ms, and fade_out_ms (0–60000).
Omitted sound properties retain the current entry's values. icon: null restores audio_lines.
Other fields cannot be null. Unknown fields, including nested sound fields, are rejected.
Nested updates require matching current binaries; after updating them, run tufo server restart.

Icons (Lucide names in snake_case): audio_lines, anchor, axe, beer, bell, bird,
bomb, bone, bow_arrow, bug, building, castle, cat, church, clock, cloud_fog,
cloud_lightning, cloud_rain, cloud_snow, coffee, coins, crown, dices, dog,
door_open, droplets, drum, fan, fish, flame, flame_kindling, flask_conical,
footprints, ghost, guitar, hammer, heart_pulse, hourglass, house, key, landmark,
moon, moon_star, mountain, mountain_snow, music, paw_print, piano, pickaxe,
rabbit, rat, scroll, shield, ship, siren, skull, snowflake, sparkles, squirrel,
sun, swords, tent, tornado, train_front, trees, venetian_mask, wand_sparkles,
waves, wind, zap."#)]
    Update {
        #[arg(value_name = "ENTRY_ID", value_parser = parse_entry_id)]
        id: CollectionEntryId,
        #[arg(value_name = "JSON_PAYLOAD")]
        payload: UpdatePayload,
    },
    /// Remove an entry from its collection.
    Delete {
        #[arg(value_name = "ENTRY_ID", value_parser = parse_entry_id)]
        id: CollectionEntryId,
    },
}

pub(crate) async fn run_entry(command: EntryCommand, json: bool) -> anyhow::Result<bool> {
    let client = api::connect().await?;
    match command {
        EntryCommand::List { collection_id } => {
            let snapshot = api::library_snapshot(&client).await?;
            api::emit(&entries_record(&snapshot, collection_id, "")?, json).await?;
        }
        EntryCommand::Get { id } => {
            let snapshot = api::library_snapshot(&client).await?;
            let (collection, entry) = entry_by_id(&snapshot, id)?;
            api::emit(&entry_record(collection, entry), json).await?;
        }
        EntryCommand::Search {
            query,
            collection_id,
        } => {
            let snapshot = api::library_snapshot(&client).await?;
            api::emit(&entries_record(&snapshot, collection_id, &query)?, json).await?;
        }
        EntryCommand::Update { id, payload } => {
            client.update_collection_entry(id, &payload.0).await?;
            api::ack("update", "entry", id, json).await?;
        }
        EntryCommand::Delete { id } => {
            client.delete_collection_entry(id).await?;
            api::ack("delete", "entry", id, json).await?;
        }
    }
    Ok(true)
}

fn entry_by_id(
    snapshot: &LibrarySnapshot,
    id: CollectionEntryId,
) -> anyhow::Result<(&CollectionView, &EntryView)> {
    snapshot
        .collections
        .iter()
        .find_map(|collection| {
            collection
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| (collection, entry))
        })
        .ok_or_else(|| anyhow::anyhow!("entry {id} does not exist"))
}

fn entry_record(collection: &CollectionView, entry: &EntryView) -> Value {
    let mut record = record::entry(entry);
    record["collection_id"] = json!(collection.id.to_string());
    record["collection_title"] = json!(collection.title.as_str());
    record["collection_kind"] = json!(match collection.kind {
        CollectionKind::Playlist => "playlist",
        CollectionKind::Soundboard => "soundboard",
    });
    record
}

fn entries_record(
    snapshot: &LibrarySnapshot,
    collection_id: Option<CollectionId>,
    query: &str,
) -> anyhow::Result<Value> {
    if let Some(id) = collection_id {
        collection_by_id(snapshot, id)?;
    }
    let records = snapshot
        .collections
        .iter()
        .filter(|collection| collection_id.is_none_or(|id| collection.id == id))
        .flat_map(|collection| {
            collection
                .entries
                .iter()
                .map(move |entry| (collection, entry))
        })
        .filter(|(collection, entry)| {
            matches_query(
                &format!(
                    "{} {} {}",
                    entry.title,
                    entry.path.as_ref().display(),
                    collection.title
                ),
                query,
            )
        })
        .map(|(collection, entry)| entry_record(collection, entry))
        .collect();
    Ok(Value::Array(records))
}

#[derive(Debug, Clone)]
pub(crate) struct UpdatePayload(UpdateCollectionEntry);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateJson {
    #[serde(default, deserialize_with = "deserialize_present")]
    title: Option<String>,
    #[serde(default)]
    icon: NullableField<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    sound: Option<SoundJson>,
}

impl FromStr for UpdatePayload {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let input: UpdateJson = crate::input::parse_json(text)?;
        Ok(Self(UpdateCollectionEntry {
            title: input
                .title
                .map(Title::try_new)
                .transpose()
                .map_err(|error| format!("invalid title: {error}"))?,
            icon: match input.icon {
                NullableField::Omitted => None,
                NullableField::Clear => Some(SoundIcon::default()),
                NullableField::Set(icon) => Some(parse_icon(&icon)?),
            },
            sound_patch: input.sound.map(SoundSettingsPatch::try_from).transpose()?,
            ..UpdateCollectionEntry::default()
        }))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SoundJson {
    #[serde(default, deserialize_with = "deserialize_present")]
    looping: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_present")]
    volume: Option<f32>,
    #[serde(default, deserialize_with = "deserialize_present")]
    fade_in_ms: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_present")]
    fade_out_ms: Option<u64>,
}

impl TryFrom<SoundJson> for SoundSettingsPatch {
    type Error = String;

    fn try_from(input: SoundJson) -> Result<Self, Self::Error> {
        Ok(Self {
            looping: input.looping,
            volume: input
                .volume
                .map(Volume::try_new)
                .transpose()
                .map_err(|error| format!("invalid sound.volume: {error}"))?,
            fade_in: input
                .fade_in_ms
                .map(parse_fade)
                .transpose()
                .map_err(|error| format!("invalid sound.fade_in_ms: {error}"))?,
            fade_out: input
                .fade_out_ms
                .map(parse_fade)
                .transpose()
                .map_err(|error| format!("invalid sound.fade_out_ms: {error}"))?,
        })
    }
}

fn parse_fade(milliseconds: u64) -> Result<FadeDuration, String> {
    FadeDuration::try_new(Duration::from_millis(milliseconds)).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_sound_update_preserves_omissions_in_the_domain_patch() -> Result<(), String> {
        let payload: UpdatePayload = r#"{"sound":{"volume":0.3}}"#.parse()?;
        assert!(payload.0.title.is_none());
        assert!(payload.0.icon.is_none());
        assert!(payload.0.sound.is_none());
        let sound = payload.0.sound_patch.ok_or("sound patch was omitted")?;
        assert_eq!(
            sound.volume,
            Some(Volume::try_new(0.3).map_err(|error| error.to_string())?)
        );
        assert!(sound.looping.is_none() && sound.fade_in.is_none() && sound.fade_out.is_none());
        Ok(())
    }

    #[test]
    fn explicit_false_zero_and_icon_clearing_are_preserved() -> Result<(), String> {
        let payload: UpdatePayload = r#"{"icon":null,"sound":{"looping":false,"volume":0,"fade_in_ms":0,"fade_out_ms":60000}}"#.parse()?;
        assert_eq!(payload.0.icon, Some(SoundIcon::default()));
        assert!(payload.0.sound.is_none());
        let sound = payload.0.sound_patch.ok_or("sound patch was omitted")?;
        assert_eq!(sound.looping, Some(false));
        assert_eq!(
            sound.volume,
            Some(Volume::try_new(0.0).map_err(|error| error.to_string())?)
        );
        assert_eq!(sound.fade_in, Some(parse_fade(0)?));
        assert_eq!(sound.fade_out, Some(parse_fade(60_000)?));
        let omitted: UpdatePayload = "{}".parse()?;
        assert!(
            omitted.0.title.is_none()
                && omitted.0.icon.is_none()
                && omitted.0.sound.is_none()
                && omitted.0.sound_patch.is_none()
        );
        Ok(())
    }

    #[test]
    fn malformed_sound_updates_fail_during_parsing() {
        for payload in [
            r#"{"sound":{"volume":1.1}}"#,
            r#"{"sound":{"volume":-0.1}}"#,
            r#"{"sound":{"fade_in_ms":60001}}"#,
            r#"{"sound":{"fade_out_ms":-1}}"#,
            r#"{"sound":{"unknown":true}}"#,
            r#"{"sound":{"looping":null}}"#,
            r#"{"sound":{"volume":null}}"#,
            r#"{"sound":null}"#,
            r#"{"title":null}"#,
            r#"{"icon":"unknown"}"#,
            r#"{"extra":true}"#,
            "[]",
        ] {
            assert!(payload.parse::<UpdatePayload>().is_err(), "{payload}");
        }
    }
}
