//! Local login registration for the GUI, independent of which client changes it.

use std::{
    io,
    path::{Path, PathBuf},
};

use crate::process::Executable;

#[cfg(target_os = "linux")]
#[path = "autostart/linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "autostart/macos.rs"]
mod platform;
#[cfg(windows)]
#[path = "autostart/windows.rs"]
mod platform;

pub const AT_LOGIN_FLAG: &str = "--at-login";
#[cfg(unix)]
const APP_ID: &str = "dev.tufo.Tufo";

pub fn read_enabled() -> io::Result<bool> {
    platform::read_enabled()
}

/// Registers `tufo-gui --at-login`, even when called from `tufo`.
pub fn set_enabled(enabled: bool) -> io::Result<()> {
    if enabled {
        platform::enable(&gui_executable()?)
    } else {
        platform::disable()
    }
}

fn gui_executable() -> io::Result<PathBuf> {
    let command = Executable::Gui.command();
    let program = Path::new(command.get_program());
    if program.is_absolute() {
        return Ok(program.to_owned());
    }
    if let Some(path) = std::env::var_os("PATH") {
        for folder in std::env::split_paths(&path) {
            let candidate = folder.join(program);
            if candidate.is_file() {
                return std::fs::canonicalize(candidate);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "tufo-gui is unavailable; build it beside tufo or add it to PATH",
    ))
}
