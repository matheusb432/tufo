use std::{process::Child, time::Duration};

use anyhow::{Context as _, ensure};
use tufo_client::{ClientError, LocalEndpoint, TufoClient, server::ServerStatus};

use crate::{command::ServerCommand, doctor, render::Console};

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

pub(crate) async fn run(
    command: ServerCommand,
    console: Console,
) -> anyhow::Result<(String, bool)> {
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
        ServerCommand::Status { json } => {
            return doctor::run(console, "server", json).await;
        }
    };
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

async fn start(endpoint: &LocalEndpoint) -> anyhow::Result<&'static str> {
    if let Some(snapshot) = inspect(endpoint).await? {
        ensure!(
            snapshot.serving,
            "tufo-server is running but not healthy; run `tufo doctor`"
        );
        ensure_matching_version(&snapshot.status.context("cannot read server status; run `just update` from the Tufo checkout to replace the running server")?)?;
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
                ensure_matching_version(&snapshot.status.context("the server cannot report its status; install matching Tufo binaries with `just update`")?)?;
                // The server must outlive this CLI. Dropping Child does not terminate it.
                process.0.take();
                return Ok("Started and healthy");
            }
            if let Some(child) = process.0.as_mut()
                && let Some(status) = child.try_wait().context("check tufo-server startup")? {
                anyhow::bail!("tufo-server exited during startup ({status}); inspect tufo-server.log in Tufo's state directory and run `tufo doctor`");
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }).await.context("tufo-server did not become healthy within 15 seconds; inspect tufo-server.log in Tufo's state directory")?
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

async fn stop(endpoint: &LocalEndpoint) -> anyhow::Result<&'static str> {
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
