use std::{process::Child, time::Duration};

use anyhow::{Context as _, ensure};
use clap::Subcommand;
use tufo_client::{ClientError, LocalEndpoint, TufoClient, server::ServerStatus};

use crate::{doctor, render::Console};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(6);
const START_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) struct Snapshot {
    pub serving: bool,
    pub status: Result<ServerStatus, ClientError>,
}

/// `None` means the transport confirms no listener; failed RPCs remain errors.
pub(crate) async fn inspect(endpoint: &LocalEndpoint) -> anyhow::Result<Option<Snapshot>> {
    if !endpoint.is_listening(PROBE_TIMEOUT).await? {
        return Ok(None);
    }
    let client = TufoClient::connect_lazy(endpoint)?;
    let (serving, status) = tokio::time::timeout(HEALTH_TIMEOUT, async {
        tokio::join!(client.server_is_serving(), client.server_status())
    })
    .await
    .context("tufo-server health check timed out after 6 seconds")?;
    Ok(Some(Snapshot {
        serving: serving?,
        status,
    }))
}

#[derive(Subcommand)]
pub(crate) enum ServerCommand {
    /// Show health, version, process, uptime, and Discord status.
    Status,
    /// Start the sibling server (or tufo-server on PATH), then wait until healthy.
    Start,
    /// Stop all audio and shut down the server through its private control endpoint.
    Kill,
    /// Shut down the server, then start it and wait until healthy.
    Restart,
}

pub(crate) async fn run(
    command: ServerCommand,
    console: Console,
    json: bool,
) -> anyhow::Result<(String, bool)> {
    let action = match &command {
        ServerCommand::Start => "start",
        ServerCommand::Kill => "kill",
        ServerCommand::Restart => "restart",
        ServerCommand::Status => "status",
    };
    let (endpoint, detail) = match command {
        ServerCommand::Start => {
            let endpoint = LocalEndpoint::from_environment().context("resolve TUFO_RUNTIME_DIR")?;
            let detail = start(&endpoint).await?;
            (endpoint, detail)
        }
        ServerCommand::Kill => {
            let endpoint = LocalEndpoint::from_environment().context("resolve TUFO_RUNTIME_DIR")?;
            let detail = stop(&endpoint).await?;
            (endpoint, detail)
        }
        ServerCommand::Restart => {
            let endpoint = LocalEndpoint::from_environment().context("resolve TUFO_RUNTIME_DIR")?;
            stop(&endpoint).await?;
            start(&endpoint).await?;
            (endpoint, "Restarted and healthy")
        }
        ServerCommand::Status => {
            return doctor::run(console, "server", json).await;
        }
    };
    if json {
        return Ok((
            serde_json::to_string(&serde_json::json!({
                "action": action,
                "resource": "server",
                "id": endpoint.path().display().to_string(),
                "status": detail,
                "endpoint": endpoint.path().display().to_string(),
            }))?,
            true,
        ));
    }
    Ok((
        format!(
            "{}\n{}\n{}",
            console.heading("server"),
            console.row("OK", "Server", detail),
            console.row("INFO", "Endpoint", &endpoint.path().display().to_string())
        ),
        true,
    ))
}

/// Starts the sibling server, or reuses one that is already running and healthy.
pub(crate) async fn start(endpoint: &LocalEndpoint) -> anyhow::Result<&'static str> {
    if let Some(snapshot) = inspect(endpoint).await? {
        ensure!(
            snapshot.serving,
            "tufo-server is running but not healthy; run `tufo doctor`"
        );
        ensure_matching_version(&snapshot.status.context(concat!(
            "cannot read server status; get the latest version at ",
            releases_url!()
        ))?)?;
        return Ok("Already running and healthy");
    }
    let mut process = StartingServer(Some(
        tufo_client::process::Executable::Server
            .command()
            .spawn()
            .context("start tufo-server; install it beside tufo or put it on PATH")?,
    ));
    tokio::time::timeout(START_TIMEOUT, async {
        loop {
            if let Ok(Some(snapshot)) = inspect(endpoint).await
                && snapshot.serving {
                ensure_matching_version(&snapshot.status.context(concat!("the server cannot report its status; get the latest version at ", releases_url!()))?)?;
                // The server must outlive this CLI. Dropping Child does not terminate it.
                process.0.take();
                return Ok("Started and healthy");
            }
            if let Some(child) = process.0.as_mut()
                && let Some(status) = child.try_wait().context("check tufo-server startup")? {
                anyhow::bail!("tufo-server exited during startup ({status}); inspect tufo-server.log in the server's state directory and run `tufo doctor`");
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }).await.context("tufo-server did not become healthy within 15 seconds; inspect tufo-server.log in the server's state directory")?
}

fn ensure_matching_version(status: &ServerStatus) -> anyhow::Result<()> {
    ensure!(
        status.version == env!("CARGO_PKG_VERSION"),
        "tufo-server {} differs from tufo {}; install matching binaries and run `tufo server restart`",
        status.version,
        env!("CARGO_PKG_VERSION"),
    );
    Ok(())
}

/// Stops the server through its private control endpoint, or reports it already stopped.
pub(crate) async fn stop(endpoint: &LocalEndpoint) -> anyhow::Result<&'static str> {
    if !endpoint.is_listening(PROBE_TIMEOUT).await? {
        return Ok("Already stopped");
    }
    TufoClient::connect_lazy(endpoint)?
        .stop_server()
        .await
        .context("ask tufo-server to shut down")?;
    tokio::time::timeout(STOP_TIMEOUT, async {
        while endpoint
            .is_listening(PROBE_TIMEOUT)
            .await
            .context("check whether tufo-server stopped listening")?
        {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("tufo-server did not stop listening within 10 seconds")??;
    Ok("Stopped")
}

/// Startup failures must not leave a child that may become ready after the CLI reports failure.
struct StartingServer(Option<Child>);

impl Drop for StartingServer {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
