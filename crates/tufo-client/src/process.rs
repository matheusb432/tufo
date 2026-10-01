//! Shared detached launch configuration for the GUI and CLI.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

/// One of Tufo's executables that another one launches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Executable {
    Server,
    Gui,
}

impl Executable {
    const fn name(self) -> &'static str {
        match self {
            Self::Server => "tufo-server",
            Self::Gui => "tufo-gui",
        }
    }

    /// Prefers the executable installed beside this one, then searches PATH. A linked command,
    /// such as macOS's `tufo` linked into the app bundle, looks beside the file it links to.
    fn path(self) -> PathBuf {
        let name = format!("{}{}", self.name(), std::env::consts::EXE_SUFFIX);
        std::env::current_exe()
            .ok()
            .map(|executable| std::fs::canonicalize(&executable).unwrap_or(executable))
            .map(|executable| executable.with_file_name(&name))
            .filter(|sibling| sibling.is_file())
            .unwrap_or_else(|| PathBuf::from(name))
    }

    /// Inherits the environment, including `TUFO_RUNTIME_DIR` and `TUFO_DISCORD_CREDENTIALS`.
    /// The process logs on its own; its output never enters the launcher's terminal.
    #[must_use]
    pub fn command(self) -> Command {
        let mut command = Command::new(self.path());
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach(&mut command);
        command
    }

    #[must_use]
    pub fn attached_command(self) -> Command {
        let mut command = Command::new(self.path());
        command.stdin(Stdio::null());
        command
    }
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}
