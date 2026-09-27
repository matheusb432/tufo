//! Finds and stops the Tufo GUI and server of this user's session. Isolated instances keep
//! running: checks and `just stress` set `TUFO_RUNTIME_DIR`, and the session's never do.

use std::{
    fs,
    os::unix::fs::MetadataExt as _,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, ensure};

const PROC_ROOT: &str = "/proc";
const ISOLATION_VARIABLE_PREFIX: &[u8] = b"TUFO_RUNTIME_DIR=";
/// The server drains for up to 5 s after SIGTERM before it exits.
const EXIT_WAIT_MAX: Duration = Duration::from_secs(15);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TufoProcess {
    Gui,
    Server,
}

impl TufoProcess {
    fn from_command_name(command_name: &str) -> Option<Self> {
        match command_name {
            "tufo-gui" => Some(Self::Gui),
            "tufo-server" => Some(Self::Server),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Gui => "tufo-gui",
            Self::Server => "tufo-server",
        }
    }
}

/// Stops the session's GUI first, so it can't start the server again, then the server, which
/// shuts down gracefully on SIGTERM.
pub(super) fn stop() -> Result<()> {
    for kind in [TufoProcess::Gui, TufoProcess::Server] {
        let process_ids = session_processes(kind)?;
        stop_processes(&process_ids)?;
        for process_id in process_ids {
            println!("stopped {} (pid {process_id})", kind.name());
        }
    }
    Ok(())
}

fn session_processes(kind: TufoProcess) -> Result<Vec<u32>> {
    let user_id = fs::metadata(Path::new(PROC_ROOT).join("self"))
        .context("read the current user from /proc")?
        .uid();
    let mut process_ids = Vec::new();
    for entry in fs::read_dir(PROC_ROOT).context("list /proc")? {
        let entry = entry.context("list /proc")?;
        let Some(process_id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        else {
            continue;
        };
        // A process can exit while it's read, which skips it.
        let directory = entry.path();
        let owned = fs::metadata(&directory).is_ok_and(|metadata| metadata.uid() == user_id);
        let matches = fs::read_to_string(directory.join("comm"))
            .is_ok_and(|name| TufoProcess::from_command_name(name.trim_end()) == Some(kind));
        let isolated = fs::read(directory.join("environ"))
            .map_or(true, |environment| is_isolated(&environment));
        if owned && matches && !isolated {
            process_ids.push(process_id);
        }
    }
    Ok(process_ids)
}

fn is_isolated(environment: &[u8]) -> bool {
    environment
        .split(|&byte| byte == 0)
        .any(|variable| variable.starts_with(ISOLATION_VARIABLE_PREFIX))
}

/// Sends SIGTERM, and SIGKILL to whatever still runs after [`EXIT_WAIT_MAX`].
fn stop_processes(process_ids: &[u32]) -> Result<()> {
    if process_ids.is_empty() {
        return Ok(());
    }
    signal(process_ids, "TERM")?;
    let deadline = Instant::now() + EXIT_WAIT_MAX;
    loop {
        let running = process_ids
            .iter()
            .copied()
            .filter(|&process_id| is_running(process_id))
            .collect::<Vec<_>>();
        if running.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            eprintln!("killing processes that ignored SIGTERM: {running:?}");
            return signal(&running, "KILL");
        }
        thread::sleep(EXIT_POLL_INTERVAL);
    }
}

fn signal(process_ids: &[u32], signal_name: &str) -> Result<()> {
    let status = Command::new("kill")
        .arg(format!("-{signal_name}"))
        .args(process_ids.iter().map(u32::to_string))
        .status()
        .context("run kill")?;
    // A process may exit between the listing and the signal, which kill reports as a failure.
    ensure!(
        status.success()
            || process_ids
                .iter()
                .all(|&process_id| !is_running(process_id)),
        "kill -{signal_name} failed: {status}"
    );
    Ok(())
}

/// A zombie has exited; only its parent's bookkeeping remains.
fn is_running(process_id: u32) -> bool {
    fs::read_to_string(
        Path::new(PROC_ROOT)
            .join(process_id.to_string())
            .join("stat"),
    )
    .is_ok_and(|stat| {
        stat.rsplit_once(") ")
            .and_then(|(_, fields)| fields.chars().next())
            .is_some_and(|state| !matches!(state, 'Z' | 'X'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tufo_runtime_dir_isolates_a_process() {
        assert!(is_isolated(b"PATH=/bin\0TUFO_RUNTIME_DIR=/tmp/check\0"));
        assert!(!is_isolated(b"PATH=/bin\0TUFO_DATABASE_PATH=/tmp/x\0"));
        assert!(!is_isolated(b"MY_TUFO_RUNTIME_DIR=/tmp\0"));
    }

    #[test]
    fn stopping_ends_a_session_process_and_spares_an_isolated_one() {
        let directory = tempfile::tempdir().unwrap();
        let fake_gui = directory.path().join("tufo-gui");
        fs::copy("/usr/bin/sleep", &fake_gui).unwrap();
        let mut session = Command::new(&fake_gui)
            .arg("30")
            .env_remove("TUFO_RUNTIME_DIR")
            .spawn()
            .unwrap();
        let mut isolated = Command::new(&fake_gui)
            .arg("30")
            .env("TUFO_RUNTIME_DIR", directory.path())
            .spawn()
            .unwrap();

        let found = session_processes(TufoProcess::Gui).unwrap();
        assert!(found.contains(&session.id()));
        assert!(!found.contains(&isolated.id()));
        stop_processes(&[session.id()]).unwrap();

        assert!(!session.wait().unwrap().success());
        assert!(is_running(isolated.id()));
        isolated.kill().unwrap();
        isolated.wait().unwrap();
    }
}
