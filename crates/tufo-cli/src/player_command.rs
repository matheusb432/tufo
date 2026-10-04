use std::time::Duration;

use anyhow::Context as _;
use clap::Subcommand;
use serde_json::json;
use tufo_client::TufoClient;
use tufo_models::library::{CollectionEntryId, CollectionId};

use crate::{
    input::{parse_collection_id, parse_entry_id},
    record,
};

mod discord;
mod output;
mod settings;

pub(crate) use discord::{DiscordCommand, run_discord};
pub(crate) use output::{OutputCommand, run_output};
use settings::{LayerUpdatePayload, SettingsCommand};

#[derive(Debug, Subcommand)]
pub(crate) enum PlayerCommand {
    /// Read one complete player snapshot.
    State,
    /// Follow player snapshots until Ctrl-C or a closed output pipe; JSON is NDJSON.
    Watch,
    /// Play a collection or one of its entries.
    #[command(subcommand)]
    Play(PlayCommand),
    /// Read or update persistent player settings.
    #[command(subcommand)]
    Settings(SettingsCommand),
    /// Inspect playback underruns, output errors, and Discord timing.
    Diagnostics,
    /// Fade out playback, release the local device, and leave Discord voice.
    Standby,
}

#[derive(Debug, Subcommand)]
pub(crate) enum MusicCommand {
    /// Read the main track, extra layers, and music settings.
    State,
    /// Play a collection or a selected entry.
    #[command(subcommand)]
    Play(PlayCommand),
    /// Pause the main track and every music layer.
    Pause,
    /// Resume the main track and every music layer.
    Resume,
    /// Stop the main track and every music layer.
    Stop,
    /// Advance to the next track.
    Next,
    /// Restart the current track or return to the previous one.
    Previous,
    /// Seek the main track to a position in milliseconds.
    Seek {
        #[arg(value_name = "POSITION_MS", value_parser = parse_position_ms)]
        position: Duration,
    },
    /// Read or update music and effects settings.
    #[command(subcommand)]
    Settings(SettingsCommand),
}

#[derive(Debug, Subcommand)]
pub(crate) enum PlayCommand {
    /// Play a playlist, optionally starting at an entry.
    #[command(
        after_long_help = "Example:\n  tufo music play collection 01a0cf81-04dd-7d42-9a53-2904e3318bc7"
    )]
    Collection {
        #[arg(value_name = "COLLECTION_ID", value_parser = parse_collection_id)]
        id: CollectionId,
        /// Start at this entry in the collection.
        #[arg(long, value_name = "ENTRY_ID", value_parser = parse_entry_id)]
        entry: Option<CollectionEntryId>,
    },
    /// Play an entry in the playlist that contains it.
    Entry {
        #[arg(value_name = "ENTRY_ID", value_parser = parse_entry_id)]
        id: CollectionEntryId,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum QueueCommand {
    /// List explicitly queued entry IDs in playback order.
    List,
    /// Queue a playlist entry; start it immediately if no music is playing.
    Add {
        #[arg(value_name = "ENTRY_ID", value_parser = parse_entry_id)]
        id: CollectionEntryId,
    },
    /// Remove an explicitly queued entry.
    Remove {
        #[arg(value_name = "ENTRY_ID", value_parser = parse_entry_id)]
        id: CollectionEntryId,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum LayerCommand {
    /// List active extra music layers, including their slots and playback state.
    List,
    /// Play a playlist entry alongside the main track.
    Add {
        #[arg(value_name = "ENTRY_ID", value_parser = parse_entry_id)]
        id: CollectionEntryId,
    },
    /// Stop one extra music layer.
    Stop {
        #[arg(value_name = "ENTRY_ID", value_parser = parse_entry_id)]
        id: CollectionEntryId,
    },
    /// Change a layer's volume or looping; omitted fields keep their values.
    #[command(after_long_help = r#"Example:
  tufo layer update 01a0cf81-04dd-7d42-9a53-2904e3318bc7 '{"volume":0.4,"looping":true}'

Volume is a linear gain from 0 to 1. Use looping:false to stop repeating.
The server ignores updates to a layer that has already stopped."#)]
    Update {
        #[arg(value_name = "ENTRY_ID", value_parser = parse_entry_id)]
        id: CollectionEntryId,
        /// One JSON object with volume and/or looping.
        #[arg(value_name = "JSON_PAYLOAD")]
        payload: LayerUpdatePayload,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum EffectCommand {
    /// List active effects and the shared effects pause state.
    List,
    /// Play a soundboard entry with its saved sound settings.
    Play {
        #[arg(value_name = "ENTRY_ID", value_parser = parse_entry_id)]
        id: CollectionEntryId,
    },
    /// Stop one soundboard entry's active sound.
    Stop {
        #[arg(value_name = "ENTRY_ID", value_parser = parse_entry_id)]
        id: CollectionEntryId,
    },
    /// Pause every active effect.
    Pause,
    /// Resume paused effects.
    Resume,
    /// Resume the looping effects saved by earlier playback.
    ResumeSavedLoops,
}

pub(crate) async fn run_player(command: PlayerCommand, json: bool) -> anyhow::Result<bool> {
    let client = crate::api::connect().await?;
    match command {
        PlayerCommand::State => {
            let state = crate::api::player_state(&client).await?;
            crate::api::emit(&crate::record::player(&state), json).await?;
        }
        PlayerCommand::Watch => return crate::api::watch_player(&client, json).await,
        PlayerCommand::Play(command) => run_play(&client, command, json).await?,
        PlayerCommand::Settings(command) => settings::run(&client, command, json).await?,
        PlayerCommand::Diagnostics => {
            let diagnostics = client.player_diagnostics().await?;
            crate::api::emit(&crate::record::diagnostics(&diagnostics), json).await?;
        }
        PlayerCommand::Standby => {
            client.stand_by().await?;
            crate::api::ack("standby", "player", "current", json).await?;
        }
    }
    Ok(true)
}

pub(crate) async fn run_music(command: MusicCommand, json: bool) -> anyhow::Result<bool> {
    let client = crate::api::connect().await?;
    match command {
        MusicCommand::State => {
            let state = crate::api::player_state(&client).await?;
            crate::api::emit(&record::music_state(&state), json).await?;
        }
        MusicCommand::Play(command) => run_play(&client, command, json).await?,
        MusicCommand::Pause => {
            client.pause_music().await?;
            crate::api::ack("pause", "music", "current", json).await?;
        }
        MusicCommand::Resume => {
            client.resume_music().await?;
            crate::api::ack("resume", "music", "current", json).await?;
        }
        MusicCommand::Stop => {
            client.stop_music().await?;
            crate::api::ack("stop", "music", "current", json).await?;
        }
        MusicCommand::Next => {
            client.skip_to_next_track().await?;
            crate::api::ack("next", "music", "current", json).await?;
        }
        MusicCommand::Previous => {
            client.skip_to_previous_track().await?;
            crate::api::ack("previous", "music", "current", json).await?;
        }
        MusicCommand::Seek { position } => {
            client.seek_music(position).await?;
            crate::api::ack("seek", "music", "current", json).await?;
        }
        MusicCommand::Settings(command) => settings::run(&client, command, json).await?,
    }
    Ok(true)
}

pub(crate) async fn run_queue(command: QueueCommand, json: bool) -> anyhow::Result<bool> {
    let client = crate::api::connect().await?;
    match command {
        QueueCommand::List => {
            let state = crate::api::player_state(&client).await?;
            crate::api::emit(
                &json!({"queued_entry_ids": record::queued_entry_ids(&state.queued_entry_ids)}),
                json,
            )
            .await?;
        }
        QueueCommand::Add { id } => {
            client.add_to_queue(id).await?;
            crate::api::ack("add", "queue", id, json).await?;
        }
        QueueCommand::Remove { id } => {
            client.remove_from_queue(id).await?;
            crate::api::ack("remove", "queue", id, json).await?;
        }
    }
    Ok(true)
}

pub(crate) async fn run_layer(command: LayerCommand, json: bool) -> anyhow::Result<bool> {
    let client = crate::api::connect().await?;
    match command {
        LayerCommand::List => {
            let state = crate::api::player_state(&client).await?;
            crate::api::emit(
                &json!({"music_layers": record::music_layers(&state.music_layers)}),
                json,
            )
            .await?;
        }
        LayerCommand::Add { id } => {
            client.add_music_layer(id).await?;
            crate::api::ack("add", "layer", id, json).await?;
        }
        LayerCommand::Stop { id } => {
            client.stop_music_layer(id).await?;
            crate::api::ack("stop", "layer", id, json).await?;
        }
        LayerCommand::Update { id, payload } => {
            client.update_music_layer(id, payload.into_inner()).await?;
            crate::api::ack("update", "layer", id, json).await?;
        }
    }
    Ok(true)
}

pub(crate) async fn run_effect(command: EffectCommand, json: bool) -> anyhow::Result<bool> {
    let client = crate::api::connect().await?;
    match command {
        EffectCommand::List => {
            let state = crate::api::player_state(&client).await?;
            crate::api::emit(&json!({"sounds": record::sounds(&state.sounds), "sounds_paused": state.sounds_paused}), json).await?;
        }
        EffectCommand::Play { id } => {
            client.play_sound(id).await?;
            crate::api::ack("play", "effect", id, json).await?;
        }
        EffectCommand::Stop { id } => {
            client.stop_sound(id).await?;
            crate::api::ack("stop", "effect", id, json).await?;
        }
        EffectCommand::Pause => {
            client.pause_sounds().await?;
            crate::api::ack("pause", "effects", "all", json).await?;
        }
        EffectCommand::Resume => {
            client.resume_sounds().await?;
            crate::api::ack("resume", "effects", "all", json).await?;
        }
        EffectCommand::ResumeSavedLoops => {
            client.resume_saved_loops().await?;
            crate::api::ack("resume_saved_loops", "effects", "saved_loops", json).await?;
        }
    }
    Ok(true)
}

async fn run_play(client: &TufoClient, command: PlayCommand, json: bool) -> anyhow::Result<()> {
    match command {
        PlayCommand::Collection { id, entry } => {
            client.play_collection(id, entry).await?;
            crate::api::ack("play", "collection", id, json).await?;
        }
        PlayCommand::Entry { id } => {
            let library = crate::api::library_snapshot(client).await?;
            let collection = library
                .collections
                .iter()
                .find(|collection| collection.entries.iter().any(|entry| entry.id == id))
                .with_context(|| format!("entry {id} was not found in the library"))?;
            client.play_collection(collection.id, Some(id)).await?;
            crate::api::ack("play", "entry", id, json).await?;
        }
    }
    Ok(())
}

fn parse_position_ms(text: &str) -> Result<Duration, String> {
    text.parse::<u64>()
        .map(Duration::from_millis)
        .map_err(|_| "position must be a nonnegative integer in milliseconds".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeks_parse_milliseconds_into_durations() -> Result<(), String> {
        assert_eq!(parse_position_ms("0")?, Duration::ZERO);
        assert_eq!(parse_position_ms("1500")?, Duration::from_millis(1500));
        for text in ["-1", "1.5", "18446744073709551616"] {
            assert!(parse_position_ms(text).is_err());
        }
        Ok(())
    }
}
