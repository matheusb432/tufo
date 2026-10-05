use anyhow::Context as _;
use serde_json::{Value, json};

use super::{
    fixtures::{write_kenku, write_wav},
    support::{Session, identifier, records},
};

async fn submit(session: &Session, action: &str, payload: Value) -> anyhow::Result<Vec<Value>> {
    let payload = payload.to_string();
    session
        .ndjson(&["kenku", action, &payload, "--timeout-ms", "10000"])
        .await
}

#[tokio::test]
async fn kenku_imports_return_durable_ids_and_conflicts_require_explicit_choices()
-> anyhow::Result<()> {
    let mut session = Session::new()?;
    let folder = session.path("kenku-fixture");
    write_kenku(&folder, "CLI Combat", None)?;
    session.start().await?;
    assert!(session.json(&["kenku", "status"]).await?.is_null());
    let first_records = submit(&session, "start", json!({"kenku_folder": folder})).await?;
    let first = first_records.last().context("missing import result")?;
    let id = identifier(&first_records[0])?;
    assert_eq!(first["id"], id);
    assert_eq!(first["state"], "succeeded");
    assert_eq!(first["report"]["collections_imported"], 1);
    assert_eq!(first["report"]["discord_bot_token_imported"], false);
    assert_eq!(session.json(&["kenku", "status"]).await?, *first);

    let review_records = submit(&session, "start", json!({"kenku_folder": folder})).await?;
    let review = review_records.last().context("missing review status")?;
    let review_id = identifier(&review_records[0])?;
    assert_ne!(id, review_id);
    assert_eq!(review["id"], review_id);
    assert_eq!(review["state"], "awaiting_choices");
    let conflicts = review["conflicts"]
        .as_array()
        .context("missing conflicts")?;
    assert_eq!(conflicts.len(), 1);
    let choice = json!({
        "source_id": conflicts[0]["source_id"],
        "kind": conflicts[0]["kind"],
        "overwrite": false,
    });
    let invalid = json!({"import_id": review_id, "choices": [{
        "source_id": conflicts[0]["source_id"], "kind": conflicts[0]["kind"],
        "overwrite": true, "typo": false
    }]})
    .to_string();
    let rejected = session.failure(&["kenku", "resume", &invalid], 2).await?;
    assert!(String::from_utf8(rejected.stderr)?.contains("unknown field"));
    assert_eq!(session.json(&["kenku", "status"]).await?, *review);
    let resumed_records = submit(
        &session,
        "resume",
        json!({"import_id": review_id, "choices": [choice]}),
    )
    .await?;
    let resumed = resumed_records.last().context("missing resumed status")?;
    assert_eq!(resumed["id"], review_id);
    assert_eq!(resumed["state"], "succeeded");
    assert_eq!(resumed["report"]["collections_imported"], 0);
    assert_eq!(
        resumed["report"]["skipped_collections"],
        json!(["CLI Combat"])
    );
    assert_eq!(session.json(&["kenku", "status"]).await?, *resumed);
    let found = session
        .json(&["collection", "search", "CLI Combat"])
        .await?;
    assert_eq!(found.as_array().context("missing collections")?.len(), 1);
    let collection_id = identifier(&found[0])?;
    assert_eq!(found[0]["entries"], json!([]));

    let audio_path = session.path("audio/drums.wav");
    let audio_bytes = write_wav(&audio_path)?;
    write_kenku(&folder, "CLI Combat", Some((&audio_path, "Imported drums")))?;
    let overwrite_records = submit(&session, "start", json!({"kenku_folder": folder})).await?;
    let overwrite_review = overwrite_records
        .last()
        .context("missing overwrite review")?;
    assert_eq!(overwrite_review["state"], "awaiting_choices");
    let overwrite_id = identifier(overwrite_review)?;
    let conflict = &overwrite_review["conflicts"][0];
    let overwritten_records = submit(&session, "resume", json!({
        "import_id": overwrite_id,
        "choices": [{"source_id": conflict["source_id"], "kind": conflict["kind"], "overwrite": true}]
    })).await?;
    let overwritten = overwritten_records
        .last()
        .context("missing overwrite result")?;
    assert_eq!(overwritten["id"], overwrite_id);
    assert_eq!(overwritten["state"], "succeeded");
    assert_eq!(overwritten["report"]["collections_imported"], 1);
    assert_eq!(overwritten["report"]["files_imported"], 1);
    let collection = session.json(&["collection", "get", collection_id]).await?;
    assert_eq!(
        collection["entries"]
            .as_array()
            .context("missing overwritten entries")?
            .len(),
        1
    );
    assert_eq!(collection["entries"][0]["title"], "Imported drums");
    assert_eq!(
        collection["entries"][0]["path"],
        json!(std::fs::canonicalize(&audio_path)?)
    );
    assert_eq!(std::fs::read(&audio_path)?, audio_bytes);
    assert!(!session.path("discord.toml").exists());
    Ok(())
}

#[tokio::test]
async fn a_failed_kenku_import_survives_restart_and_retries_after_its_source_is_restored()
-> anyhow::Result<()> {
    use tufo_application::library::import_kenku_library::kenku_data::ReadKenkuDataError;

    let mut session = Session::new()?;
    session.start().await?;
    let payload = json!({"kenku_folder": session.path("missing-kenku")}).to_string();
    let output = session
        .run(&[
            "--json",
            "kenku",
            "start",
            &payload,
            "--timeout-ms",
            "10000",
        ])
        .await?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr.as_slice(), b"");
    let emitted = records(&output.stdout)?;
    let failed = emitted.last().context("missing failed import status")?;
    assert_eq!(failed["id"], emitted[0]["id"]);
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["error_reason"], ReadKenkuDataError::NOT_FOUND);
    let status = session.run(&["kenku", "status", "--json"]).await?;
    assert_eq!(status.status.code(), Some(1));
    assert_eq!(status.stderr.as_slice(), b"");
    assert_eq!(serde_json::from_slice::<Value>(&status.stdout)?, *failed);
    session.restart().await?;
    let persisted = session.run(&["kenku", "status", "--json"]).await?;
    assert_eq!(persisted.status.code(), Some(1));
    assert_eq!(persisted.stderr.as_slice(), b"");
    assert_eq!(serde_json::from_slice::<Value>(&persisted.stdout)?, *failed);
    let source = session.path("missing-kenku");
    write_kenku(&source, "Restored Kenku", None)?;
    let retried_records = submit(&session, "retry", json!({"import_id": failed["id"]})).await?;
    let retried = retried_records
        .last()
        .context("missing retried import status")?;
    assert_eq!(retried["id"], failed["id"]);
    assert_eq!(retried["state"], "succeeded");
    assert!(retried["error_reason"].is_null());
    assert_eq!(retried["report"]["collections_imported"], 1);
    assert_eq!(session.json(&["kenku", "status"]).await?, *retried);
    assert_eq!(
        session
            .json(&["collection", "search", "Restored Kenku"])
            .await?
            .as_array()
            .context("missing restored collections")?
            .len(),
        1
    );
    assert!(!session.path("discord.toml").exists());
    Ok(())
}
