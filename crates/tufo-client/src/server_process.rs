//! Shared detached launch configuration for the GUI and CLI.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

/// Prefers the server installed beside this executable, then searches PATH.
fn executable() -> PathBuf {
    let name = format!("tufo-server{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .map(|executable| executable.with_file_name(&name))
        .filter(|sibling| sibling.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Inherits the environment, including `TUFO_DISCORD_CREDENTIALS`. The server logs to its state
/// directory; its output never enters the client's terminal.
#[must_use]
pub fn command() -> Command {
    let mut command = Command::new(executable());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut command);
    command
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
