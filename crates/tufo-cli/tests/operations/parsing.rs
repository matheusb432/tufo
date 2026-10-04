use serde_json::json;

use super::support::Session;

#[tokio::test]
async fn mutation_inputs_are_parsed_before_resolving_the_server() -> anyhow::Result<()> {
    let session = Session::new()?;
    for payload in [
        "not json".to_owned(),
        "[]".to_owned(),
        "null".to_owned(),
        "{} {}".to_owned(),
        json!({"kind": "playlist", "title": "   "}).to_string(),
        json!({"kind": "playlist", "title": "x".repeat(201)}).to_string(),
        json!({"kind": "playlist", "title": "Combat", "typo": true}).to_string(),
        json!({"kind": "unknown", "title": "Combat"}).to_string(),
    ] {
        let mut command = session.command(&["--json", "collection", "add", &payload]);
        command.env("TUFO_RUNTIME_DIR", "relative");
        let output = super::support::output(command).await?;
        assert_eq!(output.status.code(), Some(2), "{payload}: {output:?}");
        assert_eq!(output.stdout.as_slice(), b"");
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains("invalid value"), "{payload}: {stderr}");
        assert!(!stderr.contains("TUFO_RUNTIME_DIR"), "{payload}: {stderr}");
    }

    let valid = session
        .failure(
            &[
                "--json",
                "collection",
                "add",
                r#"{"kind":"playlist","title":"Combat"}"#,
            ],
            1,
        )
        .await?;
    assert!(String::from_utf8(valid.stderr)?.contains("tufo server start"));
    assert!(!session.path("runtime").exists());
    assert!(!session.path("tufo.sqlite3").exists());
    assert!(!session.path("discord.toml").exists());
    Ok(())
}

#[tokio::test]
async fn identifiers_and_nested_fields_are_rejected_as_invocation_errors() -> anyhow::Result<()> {
    let session = Session::new()?;
    for arguments in [
        ["collection", "get", "not-a-uuid"].as_slice(),
        &["entry", "get", "not-a-uuid"],
        &["loop", "waveform", "not-a-uuid"],
        &["folder", "delete", "not-a-uuid"],
    ] {
        let output = session.failure(arguments, 2).await?;
        assert!(String::from_utf8(output.stderr)?.contains("invalid value"));
    }
    for (arguments, payload) in [
        (
            ["collection", "add"].as_slice(),
            json!({"kind": "playlist", "title": "Combat", "background": {"kind": "gradient", "typo": true}}),
        ),
        (
            ["player", "settings", "update"].as_slice(),
            json!({"effects_volume": 1.25}),
        ),
    ] {
        let payload = payload.to_string();
        let arguments = arguments
            .iter()
            .copied()
            .chain(std::iter::once(payload.as_str()))
            .collect::<Vec<_>>();
        let output = session.failure(&arguments, 2).await?;
        assert!(String::from_utf8(output.stderr)?.contains("invalid value"));
    }
    assert!(!session.path("runtime").exists());
    assert!(!session.path("tufo.sqlite3").exists());
    Ok(())
}

#[tokio::test]
async fn nested_preview_objects_and_kenku_choices_are_rejected_before_connection()
-> anyhow::Result<()> {
    let session = Session::new()?;
    let id = "00000000-0000-0000-0000-000000000001";
    let draft = json!({
        "source_fingerprint": "ab".repeat(32),
        "draft": {
            "timing": {"sample_rate": 8_000, "frame_count": 8_000},
            "playback_start_frame": 0, "loop_start_frame": 2_000,
            "loop_end_frame": 6_000, "crossfade_frames": 0
        },
        "mode": {"kind": "from_frame", "start_frame": 0}
    });
    let mut timing_unknown = draft.clone();
    timing_unknown["draft"]["timing"]["typo"] = json!(true);
    let mut mode_unknown = draft;
    mode_unknown["mode"]["typo"] = json!(true);
    for payload in [timing_unknown, mode_unknown] {
        let payload = payload.to_string();
        let rejected = session
            .failure(&["loop", "preview", id, &payload], 2)
            .await?;
        assert!(String::from_utf8(rejected.stderr)?.contains("unknown field"));
    }
    let choice = json!({"source_id": "playlist", "kind": "playlist", "overwrite": false});
    let payload = json!({"import_id": id, "choices": [choice.clone(), choice]}).to_string();
    let rejected = session.failure(&["kenku", "resume", &payload], 2).await?;
    assert!(String::from_utf8(rejected.stderr)?.contains("must not repeat"));
    assert!(!session.path("runtime").exists());
    assert!(!session.path("tufo.sqlite3").exists());
    Ok(())
}
