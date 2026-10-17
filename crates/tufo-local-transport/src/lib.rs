//! User-private IPC between the GUI and `tufo-server` or the browser host, and from a later
//! GUI launch to the running GUI: Unix sockets in a private directory, or named pipes only the
//! current user can open on Windows.
//!
//! Adapted from pwf's `pwf-local-transport`.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::time::Duration;

#[cfg(unix)]
pub use unix::{
    LocalEndpoint,
    gui_instance::ActivationListener,
    listener::{BindUdsError, LocalListener, OwnedSocketPath},
};
#[cfg(windows)]
pub use windows::{LocalEndpoint, LocalListener, PipeConnection, gui_instance::ActivationListener};

/// What a GUI launch found at the GUI's endpoint.
#[derive(Debug)]
pub enum GuiInstance {
    /// No other GUI runs; the listener hears later launches.
    First(ActivationListener),
    /// Another GUI runs, and this launch asked it to show its window.
    AlreadyRunning,
}

impl GuiInstance {
    /// Claims the GUI's endpoint from [`LocalEndpoint::gui_from_environment`], or asks the GUI
    /// that owns it to show its window. `probe_timeout` bounds each wait on the other GUI.
    pub async fn claim(endpoint: &LocalEndpoint, probe_timeout: Duration) -> std::io::Result<Self> {
        #[cfg(unix)]
        return unix::gui_instance::claim(endpoint, probe_timeout).await;
        #[cfg(windows)]
        return windows::gui_instance::claim(endpoint, probe_timeout).await;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("cannot access the local tufo-server endpoint")]
    LocalEndpoint(#[from] std::io::Error),
    #[error("could not connect to the local tufo-server")]
    Transport(#[from] tonic::transport::Error),
}
