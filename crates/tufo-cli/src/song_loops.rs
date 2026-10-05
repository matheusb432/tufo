use std::path::PathBuf;

use anyhow::Context as _;
use clap::Subcommand;
use tufo_client::{
    TufoClient,
    song_loops::{ImportSongLoops, SongLoopImportOutcome, SongLoopImportResult},
};

use crate::{api, render::Console};

#[derive(Subcommand)]
pub(crate) enum SongLoopsCommand {
    /// Print every saved song loop as JSON, naming the audio each was saved against.
    Export,
    /// Match each loop in a JSON document to a song by fingerprint, or by path and SHA-256, and
    /// save it. Exits 1 when a loop matches no song or fails its checks.
    Import {
        /// A document written by `tufo song-loops export`.
        #[arg(value_name = "FILE")]
        from: PathBuf,
        /// Report what importing would change without saving anything.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite different saved loops instead of reporting them as conflicts.
        #[arg(long)]
        replace: bool,
    },
}

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
}

pub(crate) async fn run(
    command: SongLoopsCommand,
    console: Console,
    json: bool,
) -> anyhow::Result<(String, bool)> {
    let client = api::connect().await?;
    match command {
        SongLoopsCommand::Export => Ok((client.export_song_loops().await?, true)),
        SongLoopsCommand::Import {
            from,
            dry_run,
            replace,
        } => import(&client, from, dry_run, replace, json, console).await,
    }
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
    let all_matched = results.iter().all(|result| !result.outcome.is_failure());
    let output = if json {
        let songs = results.iter().map(imported_song).collect::<Vec<_>>();
        serde_json::to_string_pretty(&songs)?
    } else {
        render(&results, dry_run, console)
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

fn badge(outcome: SongLoopImportOutcome) -> &'static str {
    if outcome.is_failure() {
        "FAIL"
    } else if outcome == SongLoopImportOutcome::Conflict {
        "WARN"
    } else {
        "OK"
    }
}

fn render(results: &[SongLoopImportResult], dry_run: bool, console: Console) -> String {
    let section = if dry_run {
        "song loops (dry run)"
    } else {
        "song loops"
    };
    let mut output = console.heading(section);
    for result in results {
        output.push('\n');
        output.push_str(&console.row(
            badge(result.outcome),
            ImportOutcome::from(result.outcome).label(),
            &result.path,
        ));
    }
    output
}
