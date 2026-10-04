use clap::Subcommand;
use serde_json::Value;
use tufo_models::library::{CollectionKind, LibraryFolderId};

use super::parse_collection_kind;
use crate::{api, input::parse_folder_id, record};

#[derive(Subcommand)]
pub(crate) enum FolderCommand {
    /// List followed folders with stable IDs, paths, and collection kinds.
    List {
        /// Limit results to playlist or soundboard folders.
        #[arg(long, value_parser = parse_collection_kind)]
        kind: Option<CollectionKind>,
    },
    /// Read a followed folder by its stable ID.
    Get {
        #[arg(value_name = "FOLDER_ID", value_parser = parse_folder_id)]
        id: LibraryFolderId,
    },
    /// Stop following a folder and remove its linked collections.
    #[command(visible_alias = "delete")]
    Unfollow {
        #[arg(value_name = "FOLDER_ID", value_parser = parse_folder_id)]
        id: LibraryFolderId,
    },
}

pub(crate) async fn run_folder(command: FolderCommand, json: bool) -> anyhow::Result<bool> {
    let client = api::connect().await?;
    match command {
        FolderCommand::List { kind } => {
            let snapshot = api::library_snapshot(&client).await?;
            let folders = snapshot
                .library_folders
                .iter()
                .filter(|folder| kind.is_none_or(|kind| folder.kind == kind))
                .map(record::folder)
                .collect();
            api::emit(&Value::Array(folders), json).await?;
        }
        FolderCommand::Get { id } => {
            let snapshot = api::library_snapshot(&client).await?;
            let folder = snapshot
                .library_folders
                .iter()
                .find(|folder| folder.id == id)
                .ok_or_else(|| anyhow::anyhow!("folder {id} does not exist"))?;
            api::emit(&record::folder(folder), json).await?;
        }
        FolderCommand::Unfollow { id } => {
            client.delete_library_folder(id).await?;
            api::ack("unfollow", "folder", id, json).await?;
        }
    }
    Ok(true)
}
