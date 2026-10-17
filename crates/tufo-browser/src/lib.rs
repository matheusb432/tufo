//! The browser host on Windows. Chromium's sandbox there starts from CEF's bootstrap executable,
//! installed as `tufo-browser.exe`, which loads this library, built as `tufo-browser.dll`, into
//! the host and into each of Chromium's processes. Elsewhere the executable runs the host and
//! this library is empty.

#![cfg(windows)]

mod app;
mod canvas;
mod client;
mod events;
mod host;
mod input;
mod service;
mod session;
mod tabs;
mod ui;
mod view;

use std::ffi::c_void;

pub(crate) use host::Host;

/// The entry point the bootstrap executable calls once Chromium's sandbox is set up.
#[unsafe(no_mangle)]
pub extern "C" fn RunWinMain(
    _instance: cef::sys::HINSTANCE,
    _command_line: *const u16,
    _show_command: i32,
    sandbox_info: *mut c_void,
    _version_info: *mut c_void,
) -> i32 {
    host::run(sandbox_info.cast())
}
