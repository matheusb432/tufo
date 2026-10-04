use std::{path::Path, process::Stdio, time::Duration};

use anyhow::Context as _;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncReadExt as _, BufReader},
    net::{UnixListener, UnixStream},
    sync::oneshot,
    task::{JoinHandle, JoinSet},
};
use tufo_local_transport::{LocalEndpoint, LocalListener, OwnedSocketPath};

use super::{
    cancellation::PreviewFixture,
    support::{TEST_TIMEOUT, read_file},
};

const FAILURE_TIMEOUT: Duration = Duration::from_secs(5);

struct ConnectionRelay {
    disconnect: Option<oneshot::Sender<()>>,
    task: JoinHandle<anyhow::Result<()>>,
}

impl ConnectionRelay {
    async fn start(runtime: &Path, upstream: &Path) -> anyhow::Result<Self> {
        let endpoint = LocalEndpoint::from_root(runtime)?;
        let listener = LocalListener::bind(&endpoint, TEST_TIMEOUT).await?;
        let (incoming, ownership) = listener.into_parts();
        let listener = incoming.into_inner();
        let upstream = upstream.to_owned();
        let (disconnect, disconnected) = oneshot::channel();
        let task = tokio::spawn(relay_connections(
            listener,
            ownership,
            upstream,
            disconnected,
        ));
        Ok(Self {
            disconnect: Some(disconnect),
            task,
        })
    }

    async fn disconnect(mut self) -> anyhow::Result<()> {
        self.disconnect
            .take()
            .context("relay already disconnected")?
            .send(())
            .map_err(|()| anyhow::anyhow!("relay stopped before disconnect"))?;
        tokio::time::timeout(FAILURE_TIMEOUT, &mut self.task).await???;
        Ok(())
    }
}

impl Drop for ConnectionRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn relay_connections(
    listener: UnixListener,
    _ownership: OwnedSocketPath,
    upstream: std::path::PathBuf,
    mut disconnected: oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut disconnected => break,
            accepted = listener.accept() => {
                let (downstream, _) = accepted?;
                connections.spawn(forward_connection(downstream, upstream.clone()));
            }
        }
    }
    // Drop both ends of every accepted connection, including the active preview RPC.
    connections.abort_all();
    while let Some(result) = connections.join_next().await {
        if let Err(error) = result {
            anyhow::ensure!(error.is_cancelled(), "relay task panicked: {error}");
        }
    }
    Ok(())
}

async fn forward_connection(mut downstream: UnixStream, upstream: std::path::PathBuf) {
    if let Ok(mut upstream) = UnixStream::connect(upstream).await {
        // Probes and interrupted clients may close either side at any time.
        let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
    }
}

#[tokio::test]
async fn preview_transport_failure_exits_one_and_releases_the_pulled_preview() -> anyhow::Result<()>
{
    let mut fixture = PreviewFixture::new().await?;
    let runtime = fixture.session.path("relay");
    let upstream = LocalEndpoint::from_root(fixture.session.path("runtime"))?;
    let relay = ConnectionRelay::start(&runtime, upstream.path()).await?;
    let mut command = fixture.command();
    command.env("TUFO_RUNTIME_DIR", runtime);
    let mut stderr = tempfile::tempfile()?;
    let mut child = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let mut stdout = BufReader::new(
        child
            .stdout
            .take()
            .context("preview stdout was not piped")?,
    );
    let mut line = String::new();
    let length = tokio::time::timeout(TEST_TIMEOUT, stdout.read_line(&mut line)).await??;
    anyhow::ensure!(length > 0, "preview ended before its started record");
    let started: Value = serde_json::from_str(&line)?;
    assert_eq!(started["event"], "started");
    assert_eq!(started["audio_file_id"], fixture.audio_file_id);
    fixture
        .session
        .wait_player(|state| state.music.as_ref().is_some_and(|music| music.paused))
        .await?;

    relay.disconnect().await?;
    let status = tokio::time::timeout(FAILURE_TIMEOUT, child.wait())
        .await
        .context("preview ignored the transport failure")??;
    assert_eq!(status.code(), Some(1));
    let mut remaining = Vec::new();
    tokio::time::timeout(FAILURE_TIMEOUT, stdout.read_to_end(&mut remaining)).await??;
    assert_eq!(
        remaining.as_slice(),
        b"",
        "transport failure emitted a successful ended record"
    );
    assert_ne!(read_file(&mut stderr)?.as_slice(), b"");
    tokio::time::timeout(
        FAILURE_TIMEOUT,
        fixture.session.wait_player(|state| {
            state.music.as_ref().is_some_and(|music| {
                !music.paused && music.entry.entry_id.to_string() == fixture.entry_id
            })
        }),
    )
    .await
    .context("transport failure retained the preview mixer")??;
    assert!(fixture.session.client()?.server_is_serving().await?);
    fixture.session.json(&["music", "stop"]).await?;
    fixture.session.stop_pulled().await?;
    Ok(())
}
