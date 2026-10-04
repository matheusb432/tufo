use clap::Subcommand;
use serde_json::Value;
use tufo_models::library::AudioFileId;

use super::matches_query;
use crate::{api, input::parse_audio_file_id, record};

#[derive(Subcommand)]
pub(crate) enum SongCommand {
    /// List songs with their audio file IDs, paths, and sources.
    List,
    /// Read a song by its audio file ID.
    Get {
        #[arg(value_name = "AUDIO_FILE_ID", value_parser = parse_audio_file_id)]
        id: AudioFileId,
    },
    /// Find songs whose title or path contains every query word, ignoring case.
    Search { query: String },
    /// Remove a file source from Songs; a followed folder must be unfollowed instead.
    Delete {
        #[arg(value_name = "AUDIO_FILE_ID", value_parser = parse_audio_file_id)]
        id: AudioFileId,
    },
}

pub(crate) async fn run_song(command: SongCommand, json: bool) -> anyhow::Result<bool> {
    let client = api::connect().await?;
    match command {
        SongCommand::List => {
            let snapshot = api::library_snapshot(&client).await?;
            api::emit(
                &Value::Array(snapshot.songs.iter().map(record::song).collect()),
                json,
            )
            .await?;
        }
        SongCommand::Get { id } => {
            let snapshot = api::library_snapshot(&client).await?;
            let song = snapshot
                .songs
                .iter()
                .find(|song| song.audio_file_id == id)
                .ok_or_else(|| anyhow::anyhow!("song {id} does not exist"))?;
            api::emit(&record::song(song), json).await?;
        }
        SongCommand::Search { query } => {
            let snapshot = api::library_snapshot(&client).await?;
            let songs = snapshot
                .songs
                .iter()
                .filter(|song| {
                    matches_query(
                        &format!("{} {}", song.title, song.path.as_ref().display()),
                        &query,
                    )
                })
                .map(record::song)
                .collect();
            api::emit(&Value::Array(songs), json).await?;
        }
        SongCommand::Delete { id } => {
            client.delete_song(id).await?;
            api::ack("delete", "song", id, json).await?;
        }
    }
    Ok(true)
}
