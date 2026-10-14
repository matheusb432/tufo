#[cfg(target_os = "linux")]
#[tokio::test]
async fn concurrent_offline_desktop_updates_preserve_language_and_quit_behavior()
-> anyhow::Result<()> {
    use std::{fs::File, time::Duration};

    use anyhow::Context as _;
    use serde_json::{Value, json};
    use tufo_client::desktop::{Language, QuitBehavior, SettingsFile, TourVisibility};

    use super::support::{Session, TEST_TIMEOUT, read_file};

    let session = Session::new()?;
    session
        .mutation(
            &["desktop", "update"],
            json!({"language": "en", "keep_playing_after_quit": false, "tours": "always-hide"}),
        )
        .await?;
    let config = session.path("XDG_CONFIG_HOME/tufo/config.toml");
    let lock = File::options()
        .read(true)
        .write(true)
        .open(config.with_extension("toml.lock"))?;
    lock.lock()?;
    let mut language_stdout = tempfile::tempfile()?;
    let mut language_stderr = tempfile::tempfile()?;
    let mut quit_stdout = tempfile::tempfile()?;
    let mut quit_stderr = tempfile::tempfile()?;
    let mut language = tokio::process::Command::from(session.command(&[
        "--json",
        "desktop",
        "update",
        r#"{"language":"pt-BR"}"#,
    ]))
    .kill_on_drop(true)
    .stdout(language_stdout.try_clone()?)
    .stderr(language_stderr.try_clone()?)
    .spawn()?;
    let mut quit = tokio::process::Command::from(session.command(&[
        "--json",
        "desktop",
        "update",
        r#"{"keep_playing_after_quit":true}"#,
    ]))
    .kill_on_drop(true)
    .stdout(quit_stdout.try_clone()?)
    .stderr(quit_stderr.try_clone()?)
    .spawn()?;
    // Both processes must wait for the shared file lock before reading and merging settings.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        language.try_wait()?.is_none(),
        "language update ignored the shared lock"
    );
    assert!(
        quit.try_wait()?.is_none(),
        "quit update ignored the shared lock"
    );
    lock.unlock()?;
    let (language_status, quit_status) = tokio::time::timeout(TEST_TIMEOUT, async {
        tokio::join!(language.wait(), quit.wait())
    })
    .await
    .context("concurrent desktop updates did not finish after unlocking")?;
    assert_eq!(language_status?.code(), Some(0));
    assert_eq!(quit_status?.code(), Some(0));
    assert_eq!(read_file(&mut language_stderr)?.as_slice(), b"");
    assert_eq!(read_file(&mut quit_stderr)?.as_slice(), b"");
    let language_record: Value = serde_json::from_slice(&read_file(&mut language_stdout)?)?;
    let quit_record: Value = serde_json::from_slice(&read_file(&mut quit_stdout)?)?;
    let acknowledgment = json!({"action": "update", "resource": "desktop", "id": "local"});
    assert_eq!(language_record, acknowledgment);
    assert_eq!(quit_record, acknowledgment);
    let settings = SettingsFile::at(config).load()?;
    assert_eq!(settings.language, Language::PortugueseBrazil);
    assert_eq!(settings.quit_behavior, QuitBehavior::KeepPlaying);
    assert_eq!(settings.tours, TourVisibility::AlwaysHide);
    assert_eq!(
        session.json(&["desktop", "get"]).await?,
        json!({"language": "pt-BR", "keep_playing_after_quit": true, "tours": "always-hide", "sidebar_hidden_pages": [], "autostart": false})
    );
    assert!(
        !session
            .path("XDG_CONFIG_HOME/autostart/dev.tufo.Tufo.desktop")
            .exists()
    );
    assert!(!session.path("runtime").exists());
    assert!(!session.path("tufo.sqlite3").exists());
    Ok(())
}
