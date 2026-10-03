use std::{path::PathBuf, time::Duration};

use anyhow::{Context as _, ensure};
use tufo_client::{
    LocalEndpoint, TufoClient,
    song_loops::{ImportSongLoops, SongLoopImportOutcome, SongLoopImportResult},
};

use crate::{command::SongLoopsCommand, render::Console};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(serde::Serialize)]
struct ImportedSong {
    path: String,
    audio_file_id: Option<String>,
    outcome: ImportOutcome,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum ImportOutcome {
    Saved,
    Replaced,
    Unchanged,
    Conflict,
    NoMatch,
    IdentityMismatch,
    LengthMismatch,
    InvalidMarkers,
    Unreadable,
}

impl From<SongLoopImportOutcome> for ImportOutcome {
    fn from(outcome: SongLoopImportOutcome) -> Self {
        match outcome {
            SongLoopImportOutcome::Saved => Self::Saved,
            SongLoopImportOutcome::Replaced => Self::Replaced,
            SongLoopImportOutcome::Unchanged => Self::Unchanged,
            SongLoopImportOutcome::Conflict => Self::Conflict,
            SongLoopImportOutcome::NoMatch => Self::NoMatch,
            SongLoopImportOutcome::IdentityMismatch => Self::IdentityMismatch,
            SongLoopImportOutcome::LengthMismatch => Self::LengthMismatch,
            SongLoopImportOutcome::InvalidMarkers => Self::InvalidMarkers,
            SongLoopImportOutcome::Unreadable => Self::Unreadable,
        }
    }
}

impl ImportOutcome {
    fn label(self) -> &'static str {
        match self {
            Self::Saved => "saved",
            Self::Replaced => "replaced",
            Self::Unchanged => "unchanged",
            Self::Conflict => "conflict",
            Self::NoMatch => "no match",
            Self::IdentityMismatch => "other audio",
            Self::LengthMismatch => "other length",
            Self::InvalidMarkers => "bad markers",
            Self::Unreadable => "unreadable",
        }
    }

    fn failed(self) -> bool {
        matches!(
            self,
            Self::NoMatch
                | Self::IdentityMismatch
                | Self::LengthMismatch
                | Self::InvalidMarkers
                | Self::Unreadable
        )
    }

    fn badge(self) -> &'static str {
        match self {
            _ if self.failed() => "FAIL",
            Self::Conflict => "WARN",
            _ => "OK",
        }
    }
}

pub(crate) async fn run(
    command: SongLoopsCommand,
    console: Console,
) -> anyhow::Result<(String, bool)> {
    let client = running_server_client().await?;
    match command {
        SongLoopsCommand::Export => Ok((client.export_song_loops().await?, true)),
        SongLoopsCommand::Import {
            from,
            dry_run,
            replace,
            json,
        } => import(&client, from, dry_run, replace, json, console).await,
    }
}

async fn running_server_client() -> anyhow::Result<TufoClient> {
    let endpoint = LocalEndpoint::from_environment().context("resolve TUFO_RUNTIME_DIR")?;
    ensure!(
        endpoint.is_listening(PROBE_TIMEOUT).await?,
        "no server is running; start it with `tufo server start`"
    );
    Ok(TufoClient::connect_lazy(&endpoint)?)
}

async fn import(
    client: &TufoClient,
    from: PathBuf,
    dry_run: bool,
    replace_existing: bool,
    json: bool,
    console: Console,
) -> anyhow::Result<(String, bool)> {
    let document =
        std::fs::read_to_string(&from).with_context(|| format!("read {}", from.display()))?;
    let results = client
        .import_song_loops(&ImportSongLoops {
            document,
            dry_run,
            replace_existing,
        })
        .await?;
    let songs = results.iter().map(imported_song).collect::<Vec<_>>();
    let all_matched = songs.iter().all(|song| !song.outcome.failed());
    let output = if json {
        serde_json::to_string_pretty(&songs)?
    } else {
        render(&songs, dry_run, console)
    };
    Ok((output, all_matched))
}

fn imported_song(result: &SongLoopImportResult) -> ImportedSong {
    ImportedSong {
        path: result.path.clone(),
        audio_file_id: result.audio_file_id.map(|id| id.to_string()),
        outcome: result.outcome.into(),
    }
}

fn render(songs: &[ImportedSong], dry_run: bool, console: Console) -> String {
    let section = if dry_run {
        "song loops (dry run)"
    } else {
        "song loops"
    };
    let mut output = console.heading(section);
    for song in songs {
        output.push('\n');
        output.push_str(&console.row(song.outcome.badge(), song.outcome.label(), &song.path));
    }
    output
}
