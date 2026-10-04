use serde_json::{Value, json};
use tufo_client::library::{
    ImportReport, KenkuImportConflict, KenkuImportReport, KenkuImportStatus,
};
use tufo_models::kenku_import::KenkuImportState;

use super::collection_kind;

pub(crate) fn import_report(value: &ImportReport) -> Value {
    json!({
        "collections_created": value.collections_created,
        "files_added": value.files_added,
        "unreadable_files": value.unreadable_files.iter()
            .map(|path| path.as_ref().display().to_string()).collect::<Vec<_>>(),
        "ignored_paths": value.ignored_paths.iter()
            .map(|path| path.as_ref().display().to_string()).collect::<Vec<_>>(),
    })
}

pub(crate) fn kenku_status(value: &KenkuImportStatus) -> Value {
    json!({
        "id": value.id.to_string(),
        "state": kenku_state(value.state),
        "progress_percent": value.progress.percent(),
        "conflicts": value.conflicts.iter().map(kenku_conflict).collect::<Vec<_>>(),
        "report": kenku_report(&value.report),
        "error_reason": value.error_reason,
    })
}

fn kenku_conflict(value: &KenkuImportConflict) -> Value {
    json!({
        "source_id": value.source_id,
        "kind": collection_kind(value.kind),
        "incoming_title": value.incoming_title.as_str(),
        "existing_title": value.existing_title.as_str(),
    })
}

fn kenku_report(value: &KenkuImportReport) -> Value {
    json!({
        "collections_imported": value.collections_imported,
        "files_imported": value.files_imported,
        "skipped_collections": value.skipped_collections.iter()
            .map(tufo_models::library::Title::as_str).collect::<Vec<_>>(),
        "unreadable_files": value.unreadable_files.iter()
            .map(|path| path.as_ref().display().to_string()).collect::<Vec<_>>(),
        "unsupported_sources": value.unsupported_sources,
        "discord_bot_token_imported": value.discord_bot_token_imported,
    })
}

fn kenku_state(value: KenkuImportState) -> &'static str {
    match value {
        KenkuImportState::Reading => "reading",
        KenkuImportState::AwaitingChoices => "awaiting_choices",
        KenkuImportState::Importing => "importing",
        KenkuImportState::Succeeded => "succeeded",
        KenkuImportState::Failed => "failed",
        KenkuImportState::Interrupted => "interrupted",
    }
}
