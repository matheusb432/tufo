use std::{fmt::Display, future::Future, io::Write as _, time::Duration};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tufo_client::{
    ClientError, LibrarySnapshots, LocalEndpoint, PlayerStates, TufoClient,
    library::LibrarySnapshot,
};
use tufo_models::playback::player_state::PlayerState;

use crate::record;

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) async fn connect() -> anyhow::Result<TufoClient> {
    let endpoint = LocalEndpoint::from_environment().context("resolve TUFO_RUNTIME_DIR")?;
    ensure!(
        endpoint.is_listening(PROBE_TIMEOUT).await?,
        "no server is running; start it with `tufo server start`"
    );
    let client = TufoClient::connect_lazy(&endpoint)?;
    ensure!(
        client.server_is_serving().await?,
        "the server is not ready; inspect `tufo doctor --json`"
    );
    Ok(client)
}

pub(crate) async fn emit(value: &Value, json: bool) -> anyhow::Result<()> {
    emit_stream(value, json).await.map(|_| ())
}

pub(crate) async fn emit_stream(value: &Value, json: bool) -> anyhow::Result<bool> {
    let text = if json {
        serde_json::to_string(value)?
    } else {
        serde_json::to_string_pretty(value)?
    };
    write_text(text).await
}

pub(crate) async fn write_text(text: String) -> anyhow::Result<bool> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("tufo-stdout".to_owned())
        .spawn(move || {
            let mut stdout = std::io::stdout().lock();
            let result = match writeln!(stdout, "{text}").and_then(|()| stdout.flush()) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
                Err(error) => Err(error),
            };
            let _ = sender.send(result);
        })
        .context("start stdout writer")?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            result.context("listen for Ctrl-C")?;
            Ok(false)
        }
        result = receiver => {
            Ok(result.context("stdout writer stopped before completing")??)
        }
    }
}

pub(crate) async fn ack(
    action: &str,
    resource: &str,
    id: impl Display,
    json: bool,
) -> anyhow::Result<()> {
    emit(
        &json!({"action": action, "resource": resource, "id": id.to_string()}),
        json,
    )
    .await
}

pub(crate) async fn stdout_closed() -> anyhow::Result<()> {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        if stdout_is_closed()? {
            return Ok(());
        }
    }
}

#[cfg(unix)]
fn stdout_is_closed() -> std::io::Result<bool> {
    use std::os::fd::AsRawFd as _;

    let mut descriptor = libc::pollfd {
        fd: std::io::stdout().as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: poll receives one live descriptor, a valid output pointer, and a zero timeout.
    if unsafe { libc::poll(&raw mut descriptor, 1, 0) } < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(error);
    }
    Ok(descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0)
}

/// Reads the pipe's state instead of writing a probe into the output. A handle without attribute
/// access reports an open pipe until the next write fails.
#[cfg(windows)]
fn stdout_is_closed() -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::{
        Wdk::Storage::FileSystem::{
            FILE_PIPE_CLOSING_STATE, FILE_PIPE_DISCONNECTED_STATE, FILE_PIPE_LOCAL_INFORMATION,
            FilePipeLocalInformation, NtQueryInformationFile,
        },
        Win32::{
            Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType},
            System::IO::IO_STATUS_BLOCK,
        },
    };

    let handle = std::io::stdout().as_raw_handle();
    let mut io_status = IO_STATUS_BLOCK::default();
    let mut pipe = FILE_PIPE_LOCAL_INFORMATION::default();
    let pipe_bytes =
        u32::try_from(size_of::<FILE_PIPE_LOCAL_INFORMATION>()).map_err(std::io::Error::other)?;
    // SAFETY: stdout owns this handle, and the query writes only into two live outputs sized for
    // it.
    let queried = unsafe {
        GetFileType(handle) == FILE_TYPE_PIPE
            && NtQueryInformationFile(
                handle,
                &raw mut io_status,
                (&raw mut pipe).cast(),
                pipe_bytes,
                FilePipeLocalInformation,
            ) >= 0
    };
    Ok(queried
        && matches!(
            pipe.NamedPipeState,
            FILE_PIPE_CLOSING_STATE | FILE_PIPE_DISCONNECTED_STATE
        ))
}

pub(crate) async fn library_snapshot(client: &TufoClient) -> anyhow::Result<LibrarySnapshot> {
    snapshot(client.watch_library(), LibrarySnapshots::next, "library")
        .await
        .map(|(_, snapshot)| snapshot)
}

pub(crate) async fn player_state(client: &TufoClient) -> anyhow::Result<PlayerState> {
    snapshot(client.watch_player(), PlayerStates::next, "player")
        .await
        .map(|(_, state)| state)
}

pub(crate) async fn snapshot<S, T>(
    open: impl Future<Output = Result<S, ClientError>>,
    next: impl AsyncFnOnce(&mut S) -> Option<Result<T, ClientError>>,
    resource: &str,
) -> anyhow::Result<(S, T)> {
    tokio::time::timeout(SNAPSHOT_TIMEOUT, async {
        let mut stream = open.await?;
        let first = next(&mut stream).await.with_context(|| {
            format!("server ended the {resource} stream before its first snapshot")
        })??;
        Ok((stream, first))
    })
    .await
    .with_context(|| {
        format!(
            "{resource} snapshot did not arrive within {} seconds",
            SNAPSHOT_TIMEOUT.as_secs()
        )
    })?
}

pub(crate) async fn watch_library(client: &TufoClient, json: bool) -> anyhow::Result<bool> {
    watch(
        client.watch_library(),
        LibrarySnapshots::next,
        record::library,
        "library",
        json,
    )
    .await
}

pub(crate) async fn watch_player(client: &TufoClient, json: bool) -> anyhow::Result<bool> {
    watch(
        client.watch_player(),
        PlayerStates::next,
        record::player,
        "player",
        json,
    )
    .await
}

pub(crate) async fn watch<S, T>(
    open: impl Future<Output = Result<S, ClientError>>,
    mut next: impl AsyncFnMut(&mut S) -> Option<Result<T, ClientError>>,
    record: impl Fn(&T) -> Value,
    resource: &str,
    json: bool,
) -> anyhow::Result<bool> {
    let interrupt = tokio::signal::ctrl_c();
    let closed = stdout_closed();
    tokio::pin!(interrupt, closed);
    let (mut stream, mut current) = tokio::select! {
        result = &mut interrupt => {
            result.context("listen for Ctrl-C")?;
            return Ok(true);
        }
        result = &mut closed => {
            result?;
            return Ok(true);
        }
        result = snapshot(open, &mut next, resource) => result?,
    };
    loop {
        if !emit_stream(&record(&current), json).await? {
            return Ok(true);
        }
        current = tokio::select! {
            result = &mut interrupt => {
                result.context("listen for Ctrl-C")?;
                return Ok(true);
            }
            result = &mut closed => {
                result?;
                return Ok(true);
            }
            item = next(&mut stream) => match item {
                Some(item) => item?,
                None => return Ok(true),
            }
        };
    }
}
