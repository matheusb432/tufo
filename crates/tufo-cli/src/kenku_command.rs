mod input;

use std::time::Duration;

use anyhow::Context as _;
use clap::Subcommand;
use serde_json::{Value, json};
use tufo_client::{
    KenkuImportUpdates, TufoClient,
    library::{ImportKenkuLibrary, KenkuImportStatus},
};
use tufo_models::kenku_import::KenkuImportState;

use self::input::{KenkuResume, KenkuStart};
use crate::{api, record};

const IMPORT_TIMEOUT_MS: u64 = 330_000;

#[derive(Subcommand)]
pub(crate) enum KenkuCommand {
    /// Start from Kenku FM's default or explicit data folder and wait for durable review or
    /// completion.
    #[command(after_long_help = r#"Examples:
  tufo kenku start '{}' --json
  tufo kenku start '{"kenku_folder":"/srv/kenku-data"}' --json

kenku_folder must be an absolute path on the server's filesystem.
Omitting it, or using null, selects Kenku FM's default folder.
The command emits the import ID, then the durable review or terminal status.
Reading and importing are bounded by --timeout-ms. Ctrl-C detaches the wait;
the saved import continues on the server. Failed or interrupted results exit 1.
If the report imports a Discord token, `tufo server restart` applies it."#)]
    Start {
        #[arg(value_name = "JSON")]
        payload: KenkuStart,
        /// Maximum wait for review or completion in milliseconds (at most one hour).
        #[arg(long, default_value_t = IMPORT_TIMEOUT_MS, value_parser = clap::value_parser!(u64).range(1..=3_600_000))]
        timeout_ms: u64,
    },
    /// Read the latest saved import once, including conflicts, reports, and failure reasons.
    Status,
    /// Follow durable import states until Ctrl-C or a closed output pipe; JSON is NDJSON.
    Watch,
    /// Resume or retry a saved import, with explicit choices for every conflict when needed.
    #[command(
        visible_alias = "retry",
        after_long_help = r#"Examples:
  tufo kenku resume '{"import_id":"00000000-0000-0000-0000-000000000001","choices":[{"source_id":"kenku-playlist-1","kind":"playlist","overwrite":false},{"source_id":"kenku-soundboard-1","kind":"soundboard","overwrite":true}]}' --json
  tufo kenku retry '{"import_id":"00000000-0000-0000-0000-000000000001"}' --json

Copy source_id and kind from `tufo kenku status --json`. Each conflict needs
an explicit overwrite boolean: false keeps the existing collection; true replaces it.
kind is playlist or soundboard. Duplicate choices and unknown fields are rejected.
Omitted choices reuse the server's saved choices when it supports retrying them,
or return the durable review state. A new or changed conflict requires new choices.
An optional kenku_folder must match the saved import's source folder.
The command emits its import ID and waits for review or completion within --timeout-ms.
Ctrl-C detaches the wait; failed or interrupted results exit 1."#
    )]
    Resume {
        #[arg(value_name = "JSON")]
        payload: KenkuResume,
        /// Maximum wait for review or completion in milliseconds (at most one hour).
        #[arg(long, default_value_t = IMPORT_TIMEOUT_MS, value_parser = clap::value_parser!(u64).range(1..=3_600_000))]
        timeout_ms: u64,
    },
}

pub(crate) async fn run_kenku(command: KenkuCommand, json: bool) -> anyhow::Result<bool> {
    let client = api::connect().await?;
    match command {
        KenkuCommand::Start {
            payload,
            timeout_ms,
        } => submit(&client, payload.into_request(), "started", timeout_ms, json).await,
        KenkuCommand::Resume {
            payload,
            timeout_ms,
        } => submit(&client, payload.into_request(), "resumed", timeout_ms, json).await,
        KenkuCommand::Status => {
            let (_, status) = initial_status(&client).await?;
            api::emit(&status_record(status.as_ref()), json).await?;
            Ok(status.as_ref().is_none_or(|status| !failed(status.state)))
        }
        KenkuCommand::Watch => {
            api::watch(
                client.watch_kenku_import(),
                KenkuImportUpdates::next,
                |status| status_record(status.as_ref()),
                "Kenku import",
                json,
            )
            .await
        }
    }
}

async fn initial_status(
    client: &TufoClient,
) -> anyhow::Result<(KenkuImportUpdates, Option<KenkuImportStatus>)> {
    api::snapshot(
        client.watch_kenku_import(),
        KenkuImportUpdates::next,
        "Kenku import",
    )
    .await
}

async fn submit(
    client: &TufoClient,
    request: ImportKenkuLibrary,
    action: &str,
    timeout_ms: u64,
    json: bool,
) -> anyhow::Result<bool> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    let id = tokio::select! {
        interrupted = &mut interrupt => {
            interrupted?;
            return Ok(true);
        }
        started = tokio::time::timeout_at(deadline, client.import_kenku_library(&request)) => {
            started.context("timed out starting the Kenku import")??
        }
    };
    if !tokio::time::timeout_at(
        deadline,
        api::emit_stream(
            &json!({
                "action": action,
                "resource": "kenku_import",
                "id": id.to_string(),
            }),
            json,
        ),
    )
    .await
    .context("timed out writing the Kenku import acknowledgement; inspect `tufo kenku status`")??
    {
        return Ok(true);
    }
    let (mut updates, mut status) = tokio::select! {
        interrupted = &mut interrupt => {
            interrupted?;
            return Ok(true);
        }
        opened = tokio::time::timeout_at(deadline, initial_status(client)) => {
            opened.with_context(|| format!("timed out watching Kenku import {id}; inspect `tufo kenku status`"))??
        }
    };
    loop {
        if let Some(current) = &status {
            anyhow::ensure!(
                current.id == id,
                "the latest Kenku import changed to {} before {id} reached review or completion",
                current.id,
            );
            if !tokio::time::timeout_at(
                deadline,
                api::emit_stream(&record::kenku_status(current), json),
            )
            .await
            .context("timed out writing Kenku import progress; inspect `tufo kenku status`")??
            {
                return Ok(true);
            }
            if !current.state.is_running() {
                return Ok(!failed(current.state));
            }
        }
        let update = tokio::select! {
            interrupted = &mut interrupt => {
                interrupted?;
                return Ok(true);
            }
            result = api::stdout_closed() => {
                result?;
                return Ok(true);
            }
            update = tokio::time::timeout_at(deadline, updates.next()) => {
                update.with_context(|| format!("timed out waiting for Kenku import {id}; inspect `tufo kenku status`"))?
            }
        };
        status = update.with_context(|| {
            format!(
                "the server ended the stream before Kenku import {id} reached review or completion"
            )
        })??;
    }
}

fn failed(state: KenkuImportState) -> bool {
    matches!(
        state,
        KenkuImportState::Failed | KenkuImportState::Interrupted
    )
}

fn status_record(status: Option<&KenkuImportStatus>) -> Value {
    status.map_or(Value::Null, record::kenku_status)
}
