use serde_json::json;

use super::support::Session;

#[tokio::test]
async fn player_settings_preserve_omitted_fields_and_reject_invalid_updates_atomically()
-> anyhow::Result<()> {
    let mut session = Session::new()?;
    session.start().await?;
    session
        .mutation(
            &["player", "settings", "update"],
            json!({
                "volume": 0.25, "effects_volume": 0.5, "muted": true, "shuffle": true,
                "repeat": "playlist", "crossfade_ms": 500, "browser_volume": 0.5,
                "browser_muted": true
            }),
        )
        .await?;
    session
        .mutation(
            &["music", "settings", "update"],
            json!({"effects_volume": 0}),
        )
        .await?;
    let settings = session.json(&["player", "settings", "get"]).await?;
    assert_eq!(
        settings,
        json!({
            "volume": 0.25, "effects_volume": 0.0, "muted": true, "shuffle": true,
            "repeat": "playlist", "crossfade_ms": 500, "browser_volume": 0.5,
            "browser_muted": true
        })
    );
    let state = session.player().await?;
    assert_eq!(json!(state.volume.into_inner()), settings["volume"]);
    assert_eq!(
        json!(state.effects_volume.into_inner()),
        settings["effects_volume"]
    );
    assert_eq!(state.crossfade.into_inner().as_millis(), 500);
    assert!(state.muted && state.modes.shuffle);
    session
        .failure(
            &[
                "player",
                "settings",
                "update",
                r#"{"volume":0.75,"effects_volume":1.25}"#,
            ],
            2,
        )
        .await?;
    assert_eq!(session.player().await?, state);
    session
        .mutation(&["player", "settings", "update"], json!({}))
        .await?;
    assert_eq!(session.json(&["music", "settings", "get"]).await?, settings);
    session
        .mutation(
            &["player", "settings", "update"],
            json!({"muted": false, "shuffle": false, "repeat": "off", "crossfade_ms": 0}),
        )
        .await?;
    let cleared = session.json(&["player", "settings", "get"]).await?;
    assert_eq!(cleared["muted"], false);
    assert_eq!(cleared["shuffle"], false);
    assert_eq!(cleared["repeat"], "off");
    assert_eq!(cleared["crossfade_ms"], 0);
    assert_eq!(cleared["volume"], settings["volume"]);

    session.success(&["browser", "close"]).await?;
    let browser = session.json(&["browser", "state"]).await?;
    // Closing leaves the browser closed, or unavailable where no host sits beside the server.
    assert_ne!(browser["status"], "open");
    assert_eq!(
        (&browser["volume"], &browser["muted"], &browser["audible"]),
        (&json!(0.5), &json!(true), &json!(false))
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn desktop_settings_update_offline_in_the_isolated_profile() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use tufo_client::desktop::{Language, QuitBehavior, SettingsFile, SidebarPage, TourVisibility};

    let session = Session::new()?;
    let config = session.path("XDG_CONFIG_HOME/tufo/config.toml");
    let autostart = session.path("XDG_CONFIG_HOME/autostart/dev.tufo.Tufo.desktop");
    let defaults = session.json(&["desktop", "get"]).await?;
    assert_eq!(
        defaults,
        json!({"language": "system", "keep_playing_after_quit": false, "tours": "show-unseen", "sidebar_hidden_pages": [], "autostart": false})
    );
    session
        .mutation(
            &["desktop", "update"],
            json!({"language": "pt-BR", "keep_playing_after_quit": true, "tours": "always-hide", "sidebar_hidden_pages": ["browser", "songs"]}),
        )
        .await?;
    session
        .mutation(&["desktop", "update"], json!({"language": "en"}))
        .await?;
    let settings = session.json(&["desktop", "get"]).await?;
    assert_eq!(
        settings,
        json!({"language": "en", "keep_playing_after_quit": true, "tours": "always-hide", "sidebar_hidden_pages": ["songs", "browser"], "autostart": false})
    );
    let shared = SettingsFile::at(config.clone()).load()?;
    assert_eq!(shared.language, Language::English);
    assert_eq!(shared.quit_behavior, QuitBehavior::KeepPlaying);
    assert_eq!(shared.tours, TourVisibility::AlwaysHide);
    assert!(shared.sidebar_hidden_pages.contains(SidebarPage::Browser));
    let saved_bytes = std::fs::read(&config)?;
    for payload in [
        r#"{"language":"invalid","autostart":true}"#,
        r#"{"keep_playing_after_quit":null}"#,
        r#"{"tours":null}"#,
        r#"{"tours":"sometimes"}"#,
        r#"{"language":"en","typo":true}"#,
    ] {
        session.failure(&["desktop", "update", payload], 2).await?;
        assert_eq!(std::fs::read(&config)?, saved_bytes);
        assert!(!autostart.exists());
    }
    std::fs::create_dir_all(
        autostart
            .parent()
            .context("autostart fixture has no parent")?,
    )?;
    let existing_entry =
        b"[Desktop Entry]\nType=Application\nExec=/isolated/fixture/tufo-gui --at-login\n";
    std::fs::write(&autostart, existing_entry)?;
    session
        .mutation(
            &["desktop", "update"],
            json!({"keep_playing_after_quit": false}),
        )
        .await?;
    assert_eq!(std::fs::read(&autostart)?, existing_entry);
    assert_eq!(session.json(&["desktop", "get"]).await?["autostart"], true);
    session
        .mutation(&["desktop", "update"], json!({"autostart": false}))
        .await?;
    assert!(!autostart.exists());
    session
        .mutation(
            &["desktop", "update"],
            json!({"language": "system", "keep_playing_after_quit": false, "tours": "show-unseen", "sidebar_hidden_pages": []}),
        )
        .await?;
    assert_eq!(session.json(&["desktop", "get"]).await?, defaults);
    let malformed = b"language = 'invalid-language'\n";
    std::fs::write(&config, malformed)?;
    session
        .failure(
            &[
                "desktop",
                "update",
                r#"{"language":"en","autostart":false}"#,
            ],
            1,
        )
        .await?;
    assert_eq!(std::fs::read(&config)?, malformed);
    assert!(!autostart.exists());
    assert!(!session.path("runtime").exists());
    assert!(!session.path("tufo.sqlite3").exists());
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn desktop_autostart_registers_the_gui_sibling_without_launching_it() -> anyhow::Result<()> {
    use std::{os::unix::fs::PermissionsExt as _, path::Path};

    use anyhow::Context as _;
    use serde_json::Value;

    use super::support::output;

    let session = Session::new()?;
    let binary_directory = session.path("bin");
    let empty_path = session.path("empty-PATH");
    std::fs::create_dir(&binary_directory)?;
    std::fs::create_dir(&empty_path)?;
    let cli = binary_directory.join("tufo");
    let gui = binary_directory.join("tufo-gui");
    std::fs::copy(env!("CARGO_BIN_EXE_tufo"), &cli)?;
    let gui_bytes = b"#!/bin/sh\nexit 97\n";
    std::fs::write(&gui, gui_bytes)?;
    std::fs::set_permissions(&gui, std::fs::Permissions::from_mode(0o700))?;
    let registration = session.path("XDG_CONFIG_HOME/autostart/dev.tufo.Tufo.desktop");

    for enabled in [true, false] {
        let payload = json!({"autostart": enabled}).to_string();
        let mut command = session.command_at(&cli, &["--json", "desktop", "update", &payload]);
        command.env("PATH", &empty_path);
        let updated = output(command).await?;
        assert_eq!(updated.status.code(), Some(0));
        assert_eq!(updated.stderr.as_slice(), b"");
        assert_eq!(
            serde_json::from_slice::<Value>(&updated.stdout)?,
            json!({"action": "update", "resource": "desktop", "id": "local"})
        );
        assert_eq!(
            session.json(&["desktop", "get"]).await?["autostart"],
            enabled
        );
        if enabled {
            let entry = std::fs::read_to_string(&registration)?;
            let exec = entry
                .lines()
                .find_map(|line| line.strip_prefix("Exec="))
                .context("autostart registration has no Exec field")?;
            let executable = exec
                .strip_suffix(" --at-login")
                .context("GUI login flag is missing")?;
            assert_eq!(Path::new(executable.trim_matches('"')), gui.as_path());
        } else {
            assert!(!registration.exists());
        }
    }
    assert_eq!(std::fs::read(&gui)?.as_slice(), gui_bytes);
    assert!(!session.path("runtime").exists());
    assert!(!session.path("tufo.sqlite3").exists());
    Ok(())
}
