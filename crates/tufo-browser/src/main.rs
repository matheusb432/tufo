//! The browser host that tufo-server starts when someone opens the browser: Chromium, through
//! CEF, renders pages without a window for the GUI's view and hands their audio to the mixer.
//!
//! The server reads the host's events from its standard output and closes its standard input to
//! end it. The GUI views and drives pages through `BrowserService` on the host's own endpoint.
//! The same executable runs Chromium's renderer, GPU, and utility processes. Windows runs the
//! host from this package's library instead.

#[cfg(target_os = "linux")]
mod app;
#[cfg(target_os = "linux")]
mod canvas;
#[cfg(target_os = "linux")]
mod client;
#[cfg(target_os = "linux")]
mod events;
#[cfg(target_os = "linux")]
mod host;
#[cfg(target_os = "linux")]
mod input;
#[cfg(target_os = "linux")]
mod service;
#[cfg(target_os = "linux")]
mod session;
#[cfg(target_os = "linux")]
mod tabs;
#[cfg(target_os = "linux")]
mod ui;
#[cfg(target_os = "linux")]
mod view;

use std::process::ExitCode;

#[cfg(target_os = "linux")]
pub(crate) use host::Host;

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    let exit = host::run(std::ptr::null_mut());
    ExitCode::from(u8::try_from(exit).unwrap_or(1))
}

#[cfg(windows)]
fn main() -> ExitCode {
    eprintln!("the browser host starts through CEF's bootstrap executable and this package's DLL");
    ExitCode::FAILURE
}

#[cfg(not(any(target_os = "linux", windows)))]
fn main() -> ExitCode {
    eprintln!("the browser host runs only on Linux and Windows");
    ExitCode::FAILURE
}
