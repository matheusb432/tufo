use std::{path::PathBuf, str::FromStr};

use clap::Subcommand;
use serde::Deserialize;
use serde_json::{Value, json};
use tufo_client::{
    import_resources::{self, ResourceIds},
    library::{CollectionView, ImportPaths, LibrarySnapshot},
};
use tufo_models::{
    library::{CollectionId, CollectionKind},
    path::LocalPath,
};

use crate::{api, record};

mod collection;
mod entry;
mod folder;
mod song;

pub(crate) use collection::{CollectionCommand, run_collection};
pub(crate) use entry::{EntryCommand, run_entry};
pub(crate) use folder::{FolderCommand, run_folder};
pub(crate) use song::{SongCommand, run_song};

#[derive(Subcommand)]
pub(crate) enum LibraryCommand {
    /// Read one complete library snapshot and finish.
    Snapshot,
    /// Follow complete library snapshots until Ctrl-C; --json emits NDJSON.
    Watch,
    /// Import audio files or follow folders, returning counts and matching resource IDs.
    #[command(after_long_help = r#"Example:
  tufo library import '{"kind":"playlist","paths":["/srv/audio/music"]}'

kind is playlist or soundboard. paths must contain at least one absolute path.
Folders are followed recursively; files are referenced at their existing paths."#)]
    Import {
        /// One JSON object with kind and paths.
        #[arg(value_name = "JSON_PAYLOAD")]
        payload: ImportPayload,
    },
    /// Rescan followed folders and known audio paths.
    Rescan,
}

pub(crate) async fn run_library(command: LibraryCommand, json: bool) -> anyhow::Result<bool> {
    let client = api::connect().await?;
    match command {
        LibraryCommand::Snapshot => {
            let snapshot = api::library_snapshot(&client).await?;
            api::emit(&record::library(&snapshot), json).await?;
        }
        LibraryCommand::Watch => return api::watch_library(&client, json).await,
        LibraryCommand::Import { payload } => {
            let correlation = ImportPaths {
                kind: payload.0.kind,
                paths: import_resources::resolve_paths(payload.0.paths.clone()).await?,
            };
            let report = client.import_paths(&payload.0).await?;
            let after = api::library_snapshot(&client).await?;
            api::emit(
                &json!({
                    "action": "import",
                    "resource": "library",
                    "report": record::import_report(&report),
                    "resources": resource_ids(&import_resources::resources_for_paths(&after, &correlation)),
                    "revision": after.revision,
                }),
                json,
            ).await?;
        }
        LibraryCommand::Rescan => {
            client.rescan_library().await?;
            let snapshot = api::library_snapshot(&client).await?;
            api::emit(
                &json!({
                    "action": "rescan",
                    "resource": "library",
                    "revision": snapshot.revision,
                }),
                json,
            )
            .await?;
        }
    }
    Ok(true)
}

#[derive(Debug, Clone)]
pub(crate) struct ImportPayload(ImportPaths);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportJson {
    kind: String,
    paths: Vec<String>,
}

impl FromStr for ImportPayload {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let input: ImportJson = crate::input::parse_json(text)?;
        Ok(Self(ImportPaths {
            kind: parse_collection_kind(&input.kind)?,
            paths: parse_paths(input.paths)?,
        }))
    }
}

fn parse_collection_kind(text: &str) -> Result<CollectionKind, String> {
    match text {
        "playlist" => Ok(CollectionKind::Playlist),
        "soundboard" => Ok(CollectionKind::Soundboard),
        _ => Err("kind must be playlist or soundboard".to_owned()),
    }
}

fn parse_paths(paths: Vec<String>) -> Result<Vec<LocalPath>, String> {
    if paths.is_empty() {
        return Err("paths must contain at least one absolute path".to_owned());
    }
    paths
        .into_iter()
        .map(|path| {
            LocalPath::try_new(PathBuf::from(path))
                .map_err(|_| "paths must contain absolute UTF-8 paths".to_owned())
        })
        .collect()
}

fn collection_by_id(
    snapshot: &LibrarySnapshot,
    id: CollectionId,
) -> anyhow::Result<&CollectionView> {
    snapshot
        .collections
        .iter()
        .find(|collection| collection.id == id)
        .ok_or_else(|| anyhow::anyhow!("collection {id} does not exist"))
}

fn matches_query(text: &str, query: &str) -> bool {
    let text = text.to_lowercase();
    query
        .split_whitespace()
        .all(|word| text.contains(&word.to_lowercase()))
}

fn resource_ids(resources: &ResourceIds) -> Value {
    json!({
        "collection_ids": resources.collection_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "entry_ids": resources.entry_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "audio_file_ids": resources.audio_file_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "folder_ids": resources.folder_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_require_a_known_kind_and_nonempty_absolute_paths() {
        for payload in [
            r#"{"kind":"music","paths":["/srv/audio"]}"#,
            r#"{"kind":"playlist","paths":[]}"#,
            r#"{"kind":"playlist","paths":["relative"]}"#,
            r#"{"kind":"playlist","paths":["/srv/audio"],"extra":true}"#,
            "[]",
        ] {
            assert!(payload.parse::<ImportPayload>().is_err(), "{payload}");
        }
    }
}
