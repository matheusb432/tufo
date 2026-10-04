use std::{fs::File, process::Stdio, time::Duration};

use anyhow::Context as _;
use serde_json::Value;
#[cfg(unix)]
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt as _, BufReader, Lines},
    process::{Child, ChildStdout},
};

#[cfg(unix)]
use super::support::identifier;
use super::support::{Session, TEST_TIMEOUT, read_file};

pub(super) struct Watch {
    child: Child,
    lines: Option<Lines<BufReader<ChildStdout>>>,
    stderr: File,
}

impl Watch {
    pub(super) fn start(session: &Session, resource: &str) -> anyhow::Result<Self> {
        Self::spawn(session, &["--json", resource, "watch"])
    }

    pub(super) fn spawn(session: &Session, arguments: &[&str]) -> anyhow::Result<Self> {
        Self::spawn_command(session.command(arguments))
    }

    pub(super) fn spawn_command(command: std::process::Command) -> anyhow::Result<Self> {
        let mut command = tokio::process::Command::from(command);
        let stderr = tempfile::tempfile()?;
        let mut child = command
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(stderr.try_clone()?)
            .spawn()?;
        let stdout = child.stdout.take().context("watch stdout was not piped")?;
        Ok(Self {
            child,
            lines: Some(BufReader::new(stdout).lines()),
            stderr,
        })
    }

    pub(super) async fn next(&mut self) -> anyhow::Result<Value> {
        let line = tokio::time::timeout(
            TEST_TIMEOUT,
            self.lines
                .as_mut()
                .context("watch stdout is closed")?
                .next_line(),
        )
        .await?
        .with_context(|| format!("read watch: {}", self.stderr_text()))?
        .with_context(|| format!("watch ended: {}", self.stderr_text()))?;
        serde_json::from_str(&line).context("watch did not emit one JSON value per line")
    }

    fn stderr_text(&mut self) -> String {
        read_file(&mut self.stderr)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    }

    pub(super) async fn stopped(&mut self) -> anyhow::Result<()> {
        self.stopped_within(TEST_TIMEOUT).await
    }

    pub(super) async fn stopped_within(&mut self, timeout: Duration) -> anyhow::Result<()> {
        let status = tokio::time::timeout(timeout, self.child.wait())
            .await
            .context("stream command did not exit after cancellation")??;
        assert_eq!(status.code(), Some(0), "{}", self.stderr_text());
        assert_eq!(read_file(&mut self.stderr)?.as_slice(), b"");
        Ok(())
    }

    pub(super) fn close_stdout(&mut self) {
        drop(self.lines.take());
    }

    #[cfg(unix)]
    pub(super) fn interrupt(&self) -> anyhow::Result<()> {
        let process_id = i32::try_from(self.child.id().context("watch already exited")?)?;
        // SAFETY: this is the positive PID of the child owned by this test, never a process group.
        let result = unsafe { libc::kill(process_id, libc::SIGINT) };
        anyhow::ensure!(
            result == 0,
            "interrupt watch: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
}

#[cfg(unix)]
#[tokio::test]
async fn kenku_watch_can_be_cancelled_while_waiting_for_its_first_import() -> anyhow::Result<()> {
    let mut session = Session::new()?;
    session.start().await?;
    let mut watch = Watch::start(&session, "kenku")?;
    assert!(watch.next().await?.is_null());
    watch.interrupt()?;
    watch.stopped().await?;
    assert!(session.client()?.server_is_serving().await?);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn library_watch_emits_snapshots_and_ctrl_c_cancels_an_idle_stream() -> anyhow::Result<()> {
    let mut session = Session::new()?;
    session.start().await?;
    let mut watch = Watch::start(&session, "library")?;
    assert!(watch.next().await?["collections"].is_array());
    let created = session
        .mutation(
            &["collection", "add"],
            json!({"kind": "playlist", "title": "Stream encounter"}),
        )
        .await?;
    let id = identifier(&created)?;
    tokio::time::timeout(TEST_TIMEOUT, watch_collection(&mut watch, id)).await??;
    watch.interrupt()?;
    watch.stopped().await?;
    assert!(session.client()?.server_is_serving().await?);
    assert!(
        session
            .library()
            .await?
            .collections
            .iter()
            .any(|collection| collection.id.to_string() == id)
    );
    Ok(())
}

#[cfg(unix)]
async fn watch_collection(watch: &mut Watch, id: &str) -> anyhow::Result<()> {
    loop {
        let snapshot = watch.next().await?;
        let collections = snapshot["collections"]
            .as_array()
            .context("missing collections")?;
        if collections.iter().any(|collection| collection["id"] == id) {
            return Ok(());
        }
    }
}
