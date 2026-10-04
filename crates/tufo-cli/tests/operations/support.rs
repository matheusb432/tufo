use std::{
    fs::File,
    io::{Read as _, Seek as _},
    path::{Path, PathBuf},
    process::{Child, Output, Stdio},
    time::Duration,
};

use anyhow::Context as _;
use serde_json::Value;
use tufo_client::{LocalEndpoint, PlayerStates, TufoClient, library::LibrarySnapshot};
use tufo_models::playback::player_state::PlayerState;

pub(super) const TEST_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_BYTES_MAX: u64 = 4 * 1024 * 1024;

pub(super) struct Session {
    server: Option<ServerProcess>,
    pulled: Option<super::pulled::PulledServer>,
    directory: tempfile::TempDir,
}

impl Session {
    pub(super) fn new() -> anyhow::Result<Self> {
        Ok(Self {
            server: None,
            pulled: None,
            directory: tempfile::tempdir()?,
        })
    }

    pub(super) fn path(&self, relative: &str) -> PathBuf {
        self.directory.path().join(relative)
    }

    pub(super) fn command(&self, arguments: &[&str]) -> std::process::Command {
        self.command_at(Path::new(env!("CARGO_BIN_EXE_tufo")), arguments)
    }

    pub(super) fn command_at(
        &self,
        executable: &Path,
        arguments: &[&str],
    ) -> std::process::Command {
        let mut command = std::process::Command::new(executable);
        command.args(arguments);
        self.isolate(&mut command);
        command
    }

    fn isolate(&self, command: &mut std::process::Command) {
        command
            .current_dir(self.directory.path())
            .env("TUFO_RUNTIME_DIR", self.path("runtime"))
            .env("TUFO_DATABASE_PATH", self.path("tufo.sqlite3"))
            .env("TUFO_DISCORD_CREDENTIALS", self.path("discord.toml"))
            .env(
                "TUFO_DESKTOP_SETTINGS_PATH",
                self.path("XDG_CONFIG_HOME/tufo/config.toml"),
            )
            .env("NO_COLOR", "1")
            .env("RUST_LOG", "warn")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
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
    }

    pub(super) async fn start(&mut self) -> anyhow::Result<()> {
        let executable = Path::new(env!("CARGO_BIN_EXE_tufo"))
            .with_file_name(format!("tufo-server{}", std::env::consts::EXE_SUFFIX));
        anyhow::ensure!(
            executable.is_file(),
            "build tufo-server before running focused CLI operation tests"
        );
        let mut runtime_directory = std::fs::DirBuilder::new();
        runtime_directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;

            runtime_directory.mode(0o700);
        }
        runtime_directory
            .create(self.path("runtime"))
            .context("create the isolated private runtime directory")?;
        let mut command = std::process::Command::new(executable);
        self.isolate(&mut command);
        let stderr = tempfile::tempfile()?;
        let child = command
            .stdout(Stdio::null())
            .stderr(stderr.try_clone()?)
            .spawn()
            .context("start the isolated tufo-server")?;
        self.server = Some(ServerProcess { child, stderr });
        let client = self.client()?;
        let process = self.server.as_mut().context("missing server process")?;
        let ready = tokio::time::timeout(TEST_TIMEOUT, process.wait_ready(&client)).await;
        let log = read_file(&mut self.server.as_mut().context("missing server")?.stderr)?;
        ready
            .context("isolated server startup timed out")?
            .with_context(|| format!("isolated server startup: {}", String::from_utf8_lossy(&log)))
    }

    pub(super) async fn run(&self, arguments: &[&str]) -> anyhow::Result<Output> {
        output(self.command(arguments)).await
    }

    pub(super) async fn restart(&mut self) -> anyhow::Result<()> {
        self.client()?.stop_server().await?;
        let process = self
            .server
            .as_mut()
            .context("no server process to restart")?;
        tokio::time::timeout(TEST_TIMEOUT, process.wait_stopped())
            .await
            .context("isolated server did not stop before restart")??;
        drop(self.server.take());
        self.start().await
    }

    pub(super) async fn start_pulled(&mut self) -> anyhow::Result<()> {
        self.pulled = Some(
            super::pulled::PulledServer::start(
                &self.path("runtime"),
                &self.path("tufo.sqlite3"),
                self.path("discord.toml"),
            )
            .await?,
        );
        Ok(())
    }

    pub(super) async fn stop_pulled(&mut self) -> anyhow::Result<()> {
        self.pulled.take().context("no pulled server")?.stop().await
    }

    pub(super) async fn success(&self, arguments: &[&str]) -> anyhow::Result<Output> {
        let output = self.run(arguments).await?;
        anyhow::ensure!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stderr.as_slice(),
            b"",
            "{arguments:?}: unexpected stderr"
        );
        assert!(
            !output.stdout.contains(&0x1b),
            "{arguments:?}: decorated stdout"
        );
        Ok(output)
    }

    pub(super) async fn json(&self, arguments: &[&str]) -> anyhow::Result<Value> {
        let arguments = std::iter::once("--json")
            .chain(arguments.iter().copied())
            .collect::<Vec<_>>();
        let output = self.success(&arguments).await?;
        serde_json::from_slice(&output.stdout).context("CLI stdout was not a single JSON value")
    }

    pub(super) async fn mutation(
        &self,
        arguments: &[&str],
        payload: Value,
    ) -> anyhow::Result<Value> {
        let payload = payload.to_string();
        let arguments = arguments
            .iter()
            .copied()
            .chain(std::iter::once(payload.as_str()))
            .collect::<Vec<_>>();
        self.json(&arguments).await
    }

    pub(super) async fn ndjson(&self, arguments: &[&str]) -> anyhow::Result<Vec<Value>> {
        let arguments = std::iter::once("--json")
            .chain(arguments.iter().copied())
            .collect::<Vec<_>>();
        records(&self.success(&arguments).await?.stdout)
    }

    pub(super) async fn failure(&self, arguments: &[&str], code: i32) -> anyhow::Result<Output> {
        let output = self.run(arguments).await?;
        assert_eq!(
            output.status.code(),
            Some(code),
            "{arguments:?}: {output:?}"
        );
        assert_eq!(
            output.stdout.as_slice(),
            b"",
            "{arguments:?}: failure wrote stdout"
        );
        assert_ne!(
            output.stderr.as_slice(),
            b"",
            "{arguments:?}: missing error"
        );
        assert!(
            !output.stderr.contains(&0x1b),
            "{arguments:?}: decorated error"
        );
        Ok(output)
    }

    pub(super) fn client(&self) -> anyhow::Result<TufoClient> {
        Ok(TufoClient::connect_lazy(&LocalEndpoint::from_root(
            self.path("runtime"),
        )?)?)
    }

    pub(super) async fn library(&self) -> anyhow::Result<LibrarySnapshot> {
        let mut snapshots = self.client()?.watch_library().await?;
        tokio::time::timeout(TEST_TIMEOUT, snapshots.next())
            .await?
            .context("library stream ended before its snapshot")?
            .context("read library snapshot")
    }

    pub(super) async fn player(&self) -> anyhow::Result<PlayerState> {
        let mut states = self.client()?.watch_player().await?;
        tokio::time::timeout(TEST_TIMEOUT, states.next())
            .await?
            .context("player stream ended before its snapshot")?
            .context("read player snapshot")
    }

    pub(super) async fn wait_player(
        &self,
        matches: impl Fn(&PlayerState) -> bool,
    ) -> anyhow::Result<PlayerState> {
        tokio::time::timeout(TEST_TIMEOUT, wait_player_state(&self.client()?, matches))
            .await
            .context("player did not reach the expected state")?
    }
}

struct ServerProcess {
    child: Child,
    stderr: File,
}

impl ServerProcess {
    async fn wait_ready(&mut self, client: &TufoClient) -> anyhow::Result<()> {
        loop {
            anyhow::ensure!(
                self.child.try_wait()?.is_none(),
                "the isolated server exited during startup"
            );
            if matches!(
                tokio::time::timeout(Duration::from_secs(1), client.server_is_serving()).await,
                Ok(Ok(true))
            ) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_stopped(&mut self) -> anyhow::Result<()> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                anyhow::ensure!(
                    status.success(),
                    "isolated server shutdown failed: {status}"
                );
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

async fn wait_player_state(
    client: &TufoClient,
    matches: impl Fn(&PlayerState) -> bool,
) -> anyhow::Result<PlayerState> {
    let mut states: PlayerStates = client.watch_player().await?;
    loop {
        let state = states
            .next()
            .await
            .context("player stream ended before the expected state")??;
        if matches(&state) {
            return Ok(state);
        }
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) async fn output(command: std::process::Command) -> anyhow::Result<Output> {
    let arguments = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut command = tokio::process::Command::from(command);
    // Files avoid hanging on stdout inherited by detached children.
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let status = tokio::time::timeout(
        TEST_TIMEOUT,
        command
            .kill_on_drop(true)
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .status(),
    )
    .await
    .with_context(|| format!("CLI {arguments:?} timed out"))??;
    Ok(Output {
        status,
        stdout: read_file(&mut stdout)?,
        stderr: read_file(&mut stderr)?,
    })
}

pub(super) fn read_file(file: &mut File) -> anyhow::Result<Vec<u8>> {
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(OUTPUT_BYTES_MAX + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        u64::try_from(bytes.len())? <= OUTPUT_BYTES_MAX,
        "test output exceeded {OUTPUT_BYTES_MAX} bytes"
    );
    Ok(bytes)
}

pub(super) fn identifier(record: &Value) -> anyhow::Result<&str> {
    record["id"].as_str().context("CLI record has no string id")
}

pub(super) fn records(bytes: &[u8]) -> anyhow::Result<Vec<Value>> {
    let text = std::str::from_utf8(bytes)?;
    let records = text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(!records.is_empty(), "the command emitted no JSON records");
    Ok(records)
}
