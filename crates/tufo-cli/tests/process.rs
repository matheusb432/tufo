//! CLI contracts against isolated real servers. Workspace tests build both binaries; focused
//! runs need `cargo build -p tufo-server` first.

use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};

use anyhow::Context as _;
use serde_json::Value;
use tufo_client::{LocalEndpoint, TufoClient};

struct Session {
    directory: tempfile::TempDir,
}

impl Session {
    fn new() -> anyhow::Result<Self> {
        Ok(Self {
            directory: tempfile::tempdir()?,
        })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn command(&self, arguments: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_tufo"));
        command
            .args(arguments)
            .env("TUFO_RUNTIME_DIR", self.path("runtime"))
            .env("TUFO_DATABASE_PATH", self.path("tufo.sqlite3"))
            .env("TUFO_DISCORD_CREDENTIALS", self.path("discord.toml"))
            .env("NO_COLOR", "1")
            .env_remove("CLICOLOR_FORCE")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null());
        for variable in [
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "APPDATA",
            "LOCALAPPDATA",
        ] {
            command.env(variable, self.path(variable));
        }
        command
    }

    async fn run(&self, arguments: &[&str]) -> anyhow::Result<Output> {
        output(self.command(arguments)).await
    }

    async fn success(&self, arguments: &[&str]) -> anyhow::Result<Output> {
        let output = self.run(arguments).await?;
        anyhow::ensure!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        Ok(output)
    }

    async fn status(&self) -> anyhow::Result<Value> {
        Ok(serde_json::from_slice(
            &self.success(&["server", "status", "--json"]).await?.stdout,
        )?)
    }

    fn client(&self) -> anyhow::Result<TufoClient> {
        Ok(TufoClient::connect_lazy(&LocalEndpoint::from_root(
            self.path("runtime"),
        )?)?)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The CLI bounds shutdown. Cleanup also runs when an assertion fails.
        let _ = self
            .command(&["server", "kill"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

async fn output(command: std::process::Command) -> anyhow::Result<Output> {
    Ok(tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::from(command)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("CLI exceeded 30 seconds")??)
}

#[tokio::test]
async fn diagnostics_and_rejected_arguments_do_not_start_a_server() -> anyhow::Result<()> {
    let session = Session::new()?;
    for arguments in [
        ["doctor", "--json"].as_slice(),
        &["server", "status", "--json"],
    ] {
        let output = session.run(arguments).await?;
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        let report: Value = serde_json::from_slice(&output.stdout)?;
        assert!(report["server"].is_null());
        assert_eq!(report["checks"][0]["status"], "fail");
        assert_eq!(report["checks"][0]["detail"], "Not running");
    }
    let rejected = session.run(&["server", "start", "--unknown"]).await?;
    assert_eq!(rejected.status.code(), Some(2));
    assert!(rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rejected.stderr).starts_with("error:"));
    assert!(!session.path("runtime").exists());
    assert!(!session.path("tufo.sqlite3").exists());

    let mut command = session.command(&["doctor"]);
    command.env("CLICOLOR_FORCE", "1");
    let plain = output(command).await?;
    assert!(!plain.stdout.contains(&0x1b));
    assert!(!plain.stderr.contains(&0x1b));

    let mut command = session.command(&["doctor", "--json"]);
    command.env("TUFO_RUNTIME_DIR", "relative");
    let invalid = output(command).await?;
    assert_eq!(invalid.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&invalid.stdout)?;
    assert_eq!(report["checks"][0]["name"], "Endpoint");
    Ok(())
}

#[tokio::test]
async fn a_server_without_status_support_is_reported_as_incompatible_not_unreachable()
-> anyhow::Result<()> {
    let session = Session::new()?;
    let endpoint = LocalEndpoint::from_root(session.path("runtime"))?;
    let listener =
        tufo_local_transport::LocalListener::bind(&endpoint, Duration::from_secs(1)).await?;
    let (incoming, _ownership) = listener.into_parts();
    let (health, service) = tonic_health::server::health_reporter();
    health
        .set_service_status("", tonic_health::ServingStatus::Serving)
        .await;
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    // An older server's router answers health but returns UNIMPLEMENTED for the new status RPC.
    let server = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming_shutdown(incoming, async {
                let _ = stopped.await;
            }),
    );

    for arguments in [
        ["doctor", "--json"].as_slice(),
        &["server", "status", "--json"],
    ] {
        let output = session.run(arguments).await?;
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        let report: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(report["server"]["serving"], true);
        assert!(report["server"]["version"].is_null());
        let checks = report["checks"].as_array().context("missing checks")?;
        assert!(
            checks
                .iter()
                .any(|check| check["name"] == "Server" && check["status"] == "pass")
        );
        assert!(checks.iter().any(|check| {
            check["name"] == "Server API"
                && check["status"] == "fail"
                && check["action"]
                    .as_str()
                    .is_some_and(|action| action.contains("just update"))
        }));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("permissions"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("unreachable"));
    }
    let _ = shutdown.send(());
    tokio::time::timeout(Duration::from_secs(2), server).await???;
    Ok(())
}

#[tokio::test]
async fn starts_reuses_restarts_and_stops_the_selected_server() -> anyhow::Result<()> {
    let session = Session::new()?;
    let server = Path::new(env!("CARGO_BIN_EXE_tufo"))
        .with_file_name(format!("tufo-server{}", std::env::consts::EXE_SUFFIX));
    anyhow::ensure!(
        server.is_file(),
        "build tufo-server before running focused CLI tests"
    );
    session.success(&["server", "start"]).await?;
    let first = session.status().await?;
    assert_eq!(first["server"]["serving"], true);
    let first_pid = first["server"]["process_id"]
        .as_u64()
        .context("missing PID")?;
    session.success(&["server", "start"]).await?;
    assert_eq!(session.status().await?["server"]["process_id"], first_pid);
    session.success(&["server", "restart"]).await?;
    assert_ne!(session.status().await?["server"]["process_id"], first_pid);
    session.success(&["server", "kill"]).await?;
    let stopped = session.run(&["server", "status", "--json"]).await?;
    assert_eq!(stopped.status.code(), Some(1));
    assert!(serde_json::from_slice::<Value>(&stopped.stdout)?["server"].is_null());
    session.success(&["server", "kill"]).await?;
    Ok(())
}

#[tokio::test]
async fn reports_saved_and_invalid_credentials_without_exposing_the_token() -> anyhow::Result<()> {
    const TOKEN: &str = "test-only-token-MUST-NEVER-APPEAR";
    let session = Session::new()?;
    session.success(&["server", "start"]).await?;
    session
        .client()?
        .set_discord_bot_token(TOKEN.to_owned())
        .await?;

    for arguments in [
        ["doctor"].as_slice(),
        &["doctor", "--json"],
        &["server", "status", "--json"],
    ] {
        let output = session.success(arguments).await?;
        assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
        assert!(!output.stdout.contains(&0x1b));
    }
    let report = session.status().await?;
    let checks = report["checks"].as_array().context("missing checks")?;
    assert!(
        checks
            .iter()
            .any(|check| check["name"] == "Bot token" && check["status"] == "pass")
    );
    assert!(checks.iter().any(|check| {
        check["name"] == "Discord"
            && check["action"]
                .as_str()
                .is_some_and(|action| action.contains("restart"))
    }));

    std::fs::write(
        session.path("discord.toml"),
        format!("bot_token = {TOKEN}\n"),
    )?;
    for arguments in [
        ["doctor"].as_slice(),
        &["doctor", "--json"],
        &["server", "status", "--json"],
    ] {
        let output = session.run(arguments).await?;
        assert_eq!(output.status.code(), Some(1));
        assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
    }
    // A restart attempts to read the malformed file; the child's error must not enter CLI output.
    let failed = session.run(&["server", "restart"]).await?;
    assert_eq!(failed.status.code(), Some(1));
    assert!(failed.stdout.is_empty());
    let stderr = String::from_utf8(failed.stderr)?;
    assert!(!stderr.contains(TOKEN));
    assert!(stderr.starts_with("error:"));
    assert!(stderr.contains("Usage: tufo server restart"));
    assert!(
        !LocalEndpoint::from_root(session.path("runtime"))?
            .is_listening(Duration::from_secs(1))
            .await?
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn inaccessible_and_unresponsive_endpoints_are_not_treated_as_stopped() -> anyhow::Result<()>
{
    use std::os::unix::fs::PermissionsExt as _;

    let session = Session::new()?;
    std::fs::create_dir(session.path("runtime"))?;
    std::fs::set_permissions(
        session.path("runtime"),
        std::fs::Permissions::from_mode(0o755),
    )?;
    let refused = session.run(&["server", "kill"]).await?;
    assert_eq!(refused.status.code(), Some(1));
    assert!(refused.stdout.is_empty());

    std::fs::set_permissions(
        session.path("runtime"),
        std::fs::Permissions::from_mode(0o700),
    )?;
    let endpoint = LocalEndpoint::from_root(session.path("runtime"))?;
    let listener = tokio::net::UnixListener::bind(endpoint.path())?;
    let unresponsive = session.run(&["server", "start"]).await?;
    assert_eq!(unresponsive.status.code(), Some(1));
    assert!(unresponsive.stdout.is_empty());
    assert!(!session.path("tufo.sqlite3").exists());
    drop(listener);
    Ok(())
}

/// Runs `gui open` from a copy of `tufo` beside a stand-in `tufo-gui` script, with an empty
/// `PATH`, so neither the workspace's GUI nor an installed one ever starts.
#[cfg(unix)]
#[tokio::test]
async fn opening_the_gui_reports_a_new_window_a_raised_one_or_a_failed_start() -> anyhow::Result<()>
{
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir()?;
    let cli = directory.path().join("tufo");
    let gui = directory.path().join("tufo-gui");
    let empty_path = directory.path().join("empty");
    std::fs::create_dir(&empty_path)?;
    std::fs::copy(env!("CARGO_BIN_EXE_tufo"), &cli)?;
    let open = || {
        let mut command = std::process::Command::new(&cli);
        command
            .args(["gui", "open"])
            .env("PATH", &empty_path)
            .env("NO_COLOR", "1")
            .stdin(Stdio::null());
        output(command)
    };
    let stand_in = |script: &str| -> anyhow::Result<()> {
        std::fs::write(&gui, format!("#!/bin/sh\n{script}\n"))?;
        std::fs::set_permissions(&gui, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    };

    let missing = open().await?;
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("install it beside tufo"));

    stand_in("exit 3")?;
    let failed = open().await?;
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("tufo-gui exited during startup"));

    stand_in("exit 0")?;
    let raised = open().await?;
    assert!(raised.status.success());
    assert!(String::from_utf8_lossy(&raised.stdout).contains("Already running; raised"));

    stand_in("PATH=/usr/bin:/bin exec sleep 5")?;
    let opened = open().await?;
    assert!(opened.status.success());
    assert!(String::from_utf8_lossy(&opened.stdout).contains("Opened"));
    Ok(())
}
