//! The running GUI's pipe. Every connection to it asks that GUI to show its window.

use std::{io, time::Duration};

use tokio::net::windows::named_pipe::NamedPipeServer;
use windows_sys::Win32::Foundation::ERROR_NO_DATA;

use super::{LocalEndpoint, open_pipe, security};
use crate::GuiInstance;

/// Hears later GUI launches. Windows removes the pipe when its last handle closes.
#[derive(Debug)]
pub struct ActivationListener {
    endpoint: LocalEndpoint,
    server: NamedPipeServer,
}

impl ActivationListener {
    /// Waits for a later launch to ask for the window.
    pub async fn next_activation(&mut self) -> io::Result<()> {
        let connected = self.server.connect().await;
        // Keep a listening instance before releasing this one.
        let next = security::create_pipe(&self.endpoint.name, &self.endpoint.user_sid, false)?;
        drop(std::mem::replace(&mut self.server, next));
        match connected {
            // The launch connected and closed before this instance saw it, which still asks.
            Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA.cast_signed()) => Ok(()),
            connected => connected,
        }
    }
}

pub(crate) async fn claim(
    endpoint: &LocalEndpoint,
    probe_timeout: Duration,
) -> io::Result<GuiInstance> {
    match security::create_pipe(&endpoint.name, &endpoint.user_sid, true) {
        Ok(server) => Ok(GuiInstance::First(ActivationListener {
            endpoint: endpoint.clone(),
            server,
        })),
        // Only the first pipe instance can fail to create while the name is live, so reaching
        // the pipe means another GUI owns it, and opening it asks for the window.
        Err(create_error) => match open_pipe(&endpoint.name, probe_timeout).await {
            Ok(_pipe) => Ok(GuiInstance::AlreadyRunning),
            Err(_) => Err(create_error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    fn first(instance: GuiInstance) -> Option<ActivationListener> {
        match instance {
            GuiInstance::First(listener) => Some(listener),
            GuiInstance::AlreadyRunning => None,
        }
    }

    #[tokio::test]
    async fn a_later_launch_asks_the_first_for_its_window() {
        let root = tempfile::tempdir().unwrap();
        let endpoint = LocalEndpoint::gui_from_root(root.path()).unwrap();
        let mut running = first(claim(&endpoint, TEST_TIMEOUT).await.unwrap()).unwrap();

        let later = claim(&endpoint, TEST_TIMEOUT).await.unwrap();

        assert!(matches!(later, GuiInstance::AlreadyRunning));
        tokio::time::timeout(TEST_TIMEOUT, running.next_activation())
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn the_gui_and_the_server_have_distinct_pipes() {
        let gui = LocalEndpoint::gui_from_root(r"C:\tufo").unwrap();
        let server = LocalEndpoint::from_root(r"C:\tufo").unwrap();

        assert_ne!(gui.path(), server.path());
    }
}
