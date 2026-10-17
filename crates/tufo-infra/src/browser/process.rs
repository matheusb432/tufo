//! Starting the browser host executable and ending its process tree.

use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
};

use super::{BrowserLauncher, BrowserProcess, LaunchBrowserError, LaunchedBrowser};

const HOST_EXECUTABLE_NAME: &str = "tufo-browser";
#[cfg(windows)]
const WINDOWS_HOST_FOLDER_NAME: &str = "browser";
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

/// The installed host. Only Linux and Windows run the browser; elsewhere the host executable is
/// a stub that reports so.
fn installed_host() -> Option<PathBuf> {
    if !cfg!(any(target_os = "linux", windows)) {
        return None;
    }
    let name = format!("{HOST_EXECUTABLE_NAME}{}", std::env::consts::EXE_SUFFIX);
    let executable = std::env::current_exe().ok()?;
    Some(host_folder(&executable)?.join(name)).filter(|host| host.is_file())
}

/// Beside this executable, following a link to it as the GUI and CLI do.
#[cfg(unix)]
fn host_folder(executable: &Path) -> Option<PathBuf> {
    let executable = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
    executable.parent().map(Path::to_path_buf)
}

/// A folder beside this executable that also holds Chromium's runtime. Canonical Windows paths
/// carry the `\\?\` prefix, which the host would hand Chromium.
#[cfg(windows)]
fn host_folder(executable: &Path) -> Option<PathBuf> {
    executable
        .parent()
        .map(|folder| folder.join(WINDOWS_HOST_FOLDER_NAME))
}

impl BrowserLauncher for ProcessBrowserLauncher {
    fn is_available(&self) -> bool {
        self.executable.is_some()
    }

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
            let mut process = ChildProcess::new(child, None);
            process.kill();
            return Err(LaunchBrowserError::Spawn(std::io::Error::other(
                "the browser host's pipes did not open",
            )));
        };
        tracing::info!(pid = child.id(), "started the browser host");
        Ok(LaunchedBrowser {
            events: Box::new(events),
            process: Box::new(ChildProcess::new(child, stdin)),
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
    /// Without one, killing reaches only the host.
    #[cfg(windows)]
    job: Option<job::ProcessJob>,
}

impl ChildProcess {
    #[cfg(unix)]
    fn new(child: Child, stdin: Option<ChildStdin>) -> Self {
        Self { child, stdin }
    }

    #[cfg(windows)]
    fn new(child: Child, stdin: Option<ChildStdin>) -> Self {
        let job = job::ProcessJob::holding(&child)
            .inspect_err(|error| {
                tracing::warn!(%error, "cannot hold the browser host's processes in a job");
            })
            .ok();
        Self { child, stdin, job }
    }
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
        match &self.job {
            Some(job) => job.terminate(),
            None => {
                let _ = self.child.kill();
            }
        }
        if let Err(error) = self.child.wait() {
            tracing::warn!(%error, "cannot reap the browser host");
        }
    }
}

#[cfg(windows)]
mod job {
    use std::{
        os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
        process::Child,
    };

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the structure takes 144 bytes"
    )]
    const LIMITS_BYTES: u32 = size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
    const TERMINATED_EXIT_CODE: u32 = 1;

    /// A job holding the host and every process Chromium starts. Terminating it leaves nothing
    /// behind, and closing its handle, even through the server's crash, ends them all.
    pub(super) struct ProcessJob(OwnedHandle);

    impl ProcessJob {
        /// Puts `child` in a new job. The host starts no process of its own until CEF
        /// initializes, long after the job holds it.
        pub(super) fn holding(child: &Child) -> std::io::Result<Self> {
            // SAFETY: creates an unnamed job with default security.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: CreateJobObjectW returned a fresh handle this process owns.
            let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
            let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
                BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                    LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                    ..JOBOBJECT_BASIC_LIMIT_INFORMATION::default()
                },
                ..JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default()
            };
            // SAFETY: `limits` is the structure this information class takes, alive for the
            // call.
            let limited = unsafe {
                SetInformationJobObject(
                    job.0.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    LIMITS_BYTES,
                )
            };
            if limited == 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: the job and the child's process handle stay open for the call.
            if unsafe { AssignProcessToJobObject(job.0.as_raw_handle(), child.as_raw_handle()) }
                == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(job)
        }

        pub(super) fn terminate(&self) {
            // SAFETY: the job's handle stays open for the call.
            if unsafe { TerminateJobObject(self.0.as_raw_handle(), TERMINATED_EXIT_CODE) } == 0 {
                tracing::warn!(
                    error = %std::io::Error::last_os_error(),
                    "cannot end the browser host's processes"
                );
            }
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
