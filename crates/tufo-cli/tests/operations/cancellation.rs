use std::time::Duration;

use anyhow::Context as _;
use serde_json::json;
use tufo_client::library::ImportPaths;
use tufo_models::{library::CollectionKind, path::LocalPath};

use super::{fixtures::write_long_wav, streams::Watch, support::Session};

const CANCELLATION_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct PreviewFixture {
    pub(super) session: Session,
    pub(super) entry_id: String,
    pub(super) audio_file_id: String,
    payload: String,
}

impl PreviewFixture {
    pub(super) async fn new() -> anyhow::Result<Self> {
        let mut session = Session::new()?;
        let file = session.path("music/preview.wav");
        write_long_wav(&file, 32)?;
        session.start_pulled().await?;
        session
            .client()?
            .import_paths(&ImportPaths {
                kind: CollectionKind::Playlist,
                paths: vec![LocalPath::try_new(file.clone())?],
            })
            .await?;
        let snapshot = session.library().await?;
        let canonical_file = std::fs::canonicalize(&file)?;
        let entry = snapshot
            .collections
            .iter()
            .flat_map(|collection| &collection.entries)
            .find(|entry| entry.path.as_ref() == canonical_file.as_path())
            .context("synthetic preview song was not imported")?;
        let entry_id = entry.id.to_string();
        let audio_file_id = entry.audio_file_id.to_string();
        let waveform = session.json(&["loop", "waveform", &audio_file_id]).await?;
        session
            .mutation(
                &["music", "settings", "update"],
                json!({"repeat": "track", "crossfade_ms": 0}),
            )
            .await?;
        session.json(&["music", "play", "entry", &entry_id]).await?;
        session
            .wait_player(|state| state.music.as_ref().is_some_and(|music| !music.paused))
            .await?;
        let payload = json!({
            "source_fingerprint": waveform["fingerprint"],
            "draft": {
                "timing": waveform["timing"],
                "playback_start_frame": 0, "loop_start_frame": 8_000,
                "loop_end_frame": 160_000, "crossfade_frames": 0
            },
            "mode": {"kind": "from_frame", "start_frame": 4_000}
        })
        .to_string();
        Ok(Self {
            session,
            entry_id,
            audio_file_id,
            payload,
        })
    }

    pub(super) fn command(&self) -> std::process::Command {
        self.session.command(&[
            "--json",
            "loop",
            "preview",
            &self.audio_file_id,
            &self.payload,
        ])
    }
}

#[tokio::test]
async fn idle_watches_exit_when_stdout_closes_without_another_state_change() -> anyhow::Result<()> {
    let mut session = Session::new()?;
    session.start().await?;
    let library = session.library().await?;
    let player = session.player().await?;

    for resource in ["library", "player", "kenku"] {
        let mut watch = Watch::start(&session, resource)?;
        watch.next().await?;
        watch.close_stdout();
        watch.stopped_within(CANCELLATION_TIMEOUT).await?;
    }

    assert_eq!(session.library().await?, library);
    assert_eq!(session.player().await?, player);
    assert!(session.client()?.server_is_serving().await?);
    Ok(())
}

#[tokio::test]
async fn closing_preview_stdout_without_a_duration_releases_and_resumes_music() -> anyhow::Result<()>
{
    let mut fixture = PreviewFixture::new().await?;
    let mut preview = Watch::spawn_command(fixture.command())?;
    let started = preview.next().await?;
    assert_eq!(started["event"], "started");
    assert_eq!(started["audio_file_id"], fixture.audio_file_id);
    assert!(started["duration_ms"].is_null());
    fixture
        .session
        .wait_player(|state| state.music.as_ref().is_some_and(|music| music.paused))
        .await?;

    preview.close_stdout();
    preview.stopped_within(CANCELLATION_TIMEOUT).await?;
    let resumed = tokio::time::timeout(
        CANCELLATION_TIMEOUT,
        fixture.session.wait_player(|state| {
            state.music.as_ref().is_some_and(|music| {
                !music.paused && music.entry.entry_id.to_string() == fixture.entry_id
            })
        }),
    )
    .await
    .context("closing preview stdout did not resume local music")??;
    assert_eq!(
        resumed
            .music
            .context("preview discarded the previous music")?
            .entry
            .entry_id
            .to_string(),
        fixture.entry_id
    );
    assert_eq!(fixture.session.json(&["loop", "list"]).await?, json!([]));
    fixture.session.json(&["music", "stop"]).await?;
    fixture.session.stop_pulled().await?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn sigint_exits_a_watch_with_a_full_stdout_pipe_without_draining_it() -> anyhow::Result<()> {
    use std::{os::fd::AsRawFd as _, process::Stdio};

    use tufo_client::library::{Background, CreateCollection};
    use tufo_models::library::Title;

    use super::support::read_file;

    let mut session = Session::new()?;
    session.start().await?;
    let client = session.client()?;
    for index in 0..64 {
        client
            .create_collection(&CreateCollection {
                kind: CollectionKind::Playlist,
                title: Title::try_new(format!("collection-{index:03}-{}", "x".repeat(180)))?,
                background: Background::Gradient,
            })
            .await?;
    }
    let mut stderr = tempfile::tempfile()?;
    let mut child = tokio::process::Command::from(session.command(&["--json", "library", "watch"]))
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let stdout = child.stdout.take().context("watch stdout was not piped")?;
    let descriptor = stdout.as_raw_fd();
    // SAFETY: stdout owns the live pipe descriptor; fcntl changes only its buffer capacity.
    let capacity = unsafe { libc::fcntl(descriptor, libc::F_SETPIPE_SZ, 4096) };
    anyhow::ensure!(
        capacity > 0,
        "set test pipe capacity: {}",
        std::io::Error::last_os_error()
    );
    tokio::time::timeout(
        CANCELLATION_TIMEOUT,
        wait_for_full_pipe(&mut child, descriptor, capacity),
    )
    .await
    .context("library snapshot did not fill the unread stdout pipe")??;

    let process_id = i32::try_from(child.id().context("watch already exited")?)?;
    // SAFETY: this is the positive PID of the child owned by this test, never a process group.
    let interrupted = unsafe { libc::kill(process_id, libc::SIGINT) };
    anyhow::ensure!(
        interrupted == 0,
        "interrupt stalled writer: {}",
        std::io::Error::last_os_error()
    );
    let status = tokio::time::timeout(CANCELLATION_TIMEOUT, child.wait())
        .await
        .context("SIGINT did not exit while stdout remained full")??;
    assert_eq!(status.code(), Some(0));
    assert_eq!(read_file(&mut stderr)?.as_slice(), b"");
    // Keep the reader alive and unread until after the child's exit.
    drop(stdout);
    assert!(client.server_is_serving().await?);
    Ok(())
}

#[cfg(target_os = "linux")]
async fn wait_for_full_pipe(
    child: &mut tokio::process::Child,
    descriptor: std::os::fd::RawFd,
    capacity: i32,
) -> anyhow::Result<()> {
    loop {
        anyhow::ensure!(
            child.try_wait()?.is_none(),
            "watch exited before writing its snapshot"
        );
        let mut pending = 0;
        // SAFETY: the caller keeps stdout open; pending is a valid writable integer.
        let result = unsafe { libc::ioctl(descriptor, libc::FIONREAD, &raw mut pending) };
        anyhow::ensure!(
            result == 0,
            "inspect test pipe: {}",
            std::io::Error::last_os_error()
        );
        if pending >= capacity {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn preview_duration_releases_playback_with_unread_update_records() -> anyhow::Result<()> {
    use std::{num::NonZeroU32, os::fd::AsRawFd as _, process::Stdio};

    use tokio::io::AsyncBufReadExt as _;
    use tufo_client::song_loops::{PreviewId, UpdateSongLoopPreview};
    use tufo_models::library::song_loop::{LoopMarkers, LoopRegion, SourceFrames, SourceTiming};
    let mut fixture = PreviewFixture::new().await?;
    let mut command = fixture.command();
    command.args(["--duration-ms", "1800"]);
    let mut child = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().context("preview stdout missing")?;
    let descriptor = stdout.as_raw_fd();
    // SAFETY: stdout owns the live pipe descriptor; only its capacity changes.
    let capacity = unsafe { libc::fcntl(descriptor, libc::F_SETPIPE_SZ, 4096) };
    anyhow::ensure!(capacity == 4096, "could not limit the pipe capacity");
    let mut stdout = tokio::io::BufReader::new(stdout);
    let mut line = String::new();
    tokio::time::timeout(CANCELLATION_TIMEOUT, stdout.read_line(&mut line)).await??;
    let started: serde_json::Value = serde_json::from_str(&line)?;
    let preview_id = PreviewId(
        started["preview_id"]
            .as_u64()
            .context("preview id missing")?,
    );
    let client = fixture.session.client()?;
    for index in 0..25 {
        let draft = LoopRegion::try_new(
            SourceTiming {
                sample_rate: NonZeroU32::new(8_000).unwrap(),
                frame_count: SourceFrames(240_000),
            },
            LoopMarkers {
                playback_start: SourceFrames(index % 2),
                loop_start: SourceFrames(8_000),
                loop_end: SourceFrames(160_000),
                crossfade: SourceFrames::ZERO,
            },
        )?;
        client
            .update_song_loop_preview(UpdateSongLoopPreview { preview_id, draft })
            .await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let status = tokio::time::timeout(CANCELLATION_TIMEOUT, child.wait())
        .await
        .context("preview duration did not stop the command with unread update records")??;
    assert!(status.success());
    fixture
        .session
        .wait_player(|state| state.music.as_ref().is_some_and(|music| !music.paused))
        .await?;
    drop(stdout);
    fixture.session.stop_pulled().await?;
    Ok(())
}
