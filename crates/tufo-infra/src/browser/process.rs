//! Starting the browser host executable and ending its process tree.

use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
};

use super::{BrowserLauncher, BrowserProcess, LaunchBrowserError, LaunchedBrowser};

const HOST_EXECUTABLE_NAME: &str = "tufo-browser";
const PROFILE_FOLDER_NAME: &str = "browser";
const LOG_FILE_NAME: &str = "tufo-browser.log";
/// A log past this size starts over when the host starts.
const LOG_RESTART_BYTES: u64 = 10 * 1024 * 1024;
/// The server's own environment the host must not inherit.
const WITHHELD_ENVIRONMENT: [&str; 2] = ["TUFO_DISCORD_CREDENTIALS", "PIPEWIRE_CONFIG_NAME"];

/// Starts the `tufo-browser` executable installed beside this one, with a Tufo-owned profile.
#[derive(Debug, Clone)]
pub struct ProcessBrowserLauncher {
    executable: Option<PathBuf>,
    profile_directory: PathBuf,
    log_file: Option<PathBuf>,
}

impl ProcessBrowserLauncher {
    /// Keeps the profile in a `browser` folder beside the database, so isolated databases get
    /// isolated profiles, and logs to `log_directory` when given.
    #[must_use]
    pub fn beside_database(database_path: &Path, log_directory: Option<&Path>) -> Self {
        let profile_directory = database_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(PROFILE_FOLDER_NAME);
        Self {
            executable: installed_host(),
            profile_directory,
            log_file: log_directory.map(|directory| directory.join(LOG_FILE_NAME)),
        }
    }

    fn log_output(&self) -> Stdio {
        let Some(path) = &self.log_file else {
            return Stdio::null();
        };
        let restart =
            std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > LOG_RESTART_BYTES);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(!restart)
            .truncate(restart)
            .open(path);
        match file {
            Ok(file) => file.into(),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "cannot open the browser log");
                Stdio::null()
            }
        }
    }
}

/// The host beside this executable, following a link to it as the GUI and CLI do.
fn installed_host() -> Option<PathBuf> {
    let name = format!("{HOST_EXECUTABLE_NAME}{}", std::env::consts::EXE_SUFFIX);
    let executable = std::env::current_exe().ok()?;
    let executable = std::fs::canonicalize(&executable).unwrap_or(executable);
    Some(executable.with_file_name(name)).filter(|host| host.is_file())
}

impl BrowserLauncher for ProcessBrowserLauncher {
    fn launch(&self) -> Result<LaunchedBrowser, LaunchBrowserError> {
        let executable = self
            .executable
            .as_ref()
            .ok_or(LaunchBrowserError::Unavailable)?;
        std::fs::create_dir_all(&self.profile_directory).map_err(LaunchBrowserError::Spawn)?;
        let mut command = Command::new(executable);
        command
            .arg("--profile")
            .arg(&self.profile_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(self.log_output());
        for name in WITHHELD_ENVIRONMENT {
            command.env_remove(name);
        }
        own_process_group(&mut command);
        let mut child = command.spawn().map_err(LaunchBrowserError::Spawn)?;
        let (Some(events), stdin) = (child.stdout.take(), child.stdin.take()) else {
            let mut process = ChildProcess { child, stdin: None };
            process.kill();
            return Err(LaunchBrowserError::Spawn(std::io::Error::other(
                "the browser host's pipes did not open",
            )));
        };
        tracing::info!(pid = child.id(), "started the browser host");
        Ok(LaunchedBrowser {
            events: Box::new(events),
            process: Box::new(ChildProcess { child, stdin }),
        })
    }
}

/// The host leads a process group holding every process Chromium starts, so killing the group
/// leaves nothing behind.
#[cfg(unix)]
fn own_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(windows)]
fn own_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

struct ChildProcess {
    child: Child,
    /// Closing it asks the host to exit.
    stdin: Option<ChildStdin>,
}

impl BrowserProcess for ChildProcess {
    fn request_exit(&mut self) {
        self.stdin.take();
    }

    #[cfg(unix)]
    fn has_exited(&mut self) -> bool {
        // SAFETY: a zeroed siginfo_t is a valid output buffer for waitid.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let pid: libc::id_t = self.child.id();
        // WNOWAIT leaves the exited host unreaped, so its id still names its process group.
        // SAFETY: waitid only writes into `info`.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &raw mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        result != 0 || exited_process_id(&info) != 0
    }

    #[cfg(windows)]
    fn has_exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    #[cfg(unix)]
    fn kill(&mut self) {
        if let Ok(group) = libc::pid_t::try_from(self.child.id()) {
            // SAFETY: the unreaped host keeps its id, so the group is still the host's.
            unsafe { libc::killpg(group, libc::SIGKILL) };
        }
        if let Err(error) = self.child.wait() {
            tracing::warn!(%error, "cannot reap the browser host");
        }
    }

    #[cfg(windows)]
    fn kill(&mut self) {
        let _ = self.child.kill();
        if let Err(error) = self.child.wait() {
            tracing::warn!(%error, "cannot reap the browser host");
        }
    }
}

/// The id waitid reported, zero while the child still runs.
#[cfg(target_os = "macos")]
fn exited_process_id(info: &libc::siginfo_t) -> libc::pid_t {
    info.si_pid
}

#[cfg(all(unix, not(target_os = "macos")))]
fn exited_process_id(info: &libc::siginfo_t) -> libc::pid_t {
    // SAFETY: waitid filled `info` for a child-state change, which carries si_pid.
    unsafe { info.si_pid() }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn spawn(script: &str) -> std::io::Result<ChildProcess> {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::null());
        own_process_group(&mut command);
        let mut child = command.spawn()?;
        let stdin = child.stdin.take();
        Ok(ChildProcess { child, stdin })
    }

    fn wait_until_exited(process: &mut ChildProcess) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if process.has_exited() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn closing_input_asks_the_host_to_exit() {
        let mut process = spawn("cat >/dev/null").unwrap();
        assert!(!process.has_exited());

        process.request_exit();

        assert!(wait_until_exited(&mut process));
        assert!(process.has_exited(), "exit stays observable until reaped");
        process.kill();
    }

    #[test]
    fn killing_ends_every_process_in_the_group() {
        let mut process = spawn("sleep 30 & wait").unwrap();
        let group = libc::pid_t::try_from(process.child.id()).unwrap();

        process.kill();

        // The killed sleep lingers as a zombie until init reaps it.
        let deadline = Instant::now() + Duration::from_secs(5);
        // SAFETY: signal 0 only probes whether the group still has members.
        while unsafe { libc::killpg(group, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "the background sleep outlived the kill"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
