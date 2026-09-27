//! The running GUI's socket. Every connection to it asks that GUI to show its window.

use std::{io, time::Duration};

use tokio::net::UnixStream;
use tokio_stream::{StreamExt as _, wrappers::UnixListenerStream};

use super::{
    LocalEndpoint,
    listener::{BindUdsError, LocalListener, OwnedSocketPath},
};
use crate::GuiInstance;

/// Hears later GUI launches, and removes the socket when dropped.
#[derive(Debug)]
pub struct ActivationListener {
    incoming: UnixListenerStream,
    _ownership: OwnedSocketPath,
}

impl ActivationListener {
    /// Waits for a later launch to ask for the window.
    pub async fn next_activation(&mut self) -> io::Result<()> {
        match self.incoming.next().await {
            Some(connection) => connection.map(drop),
            None => Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
        }
    }
}

pub(crate) async fn claim(
    endpoint: &LocalEndpoint,
    probe_timeout: Duration,
) -> io::Result<GuiInstance> {
    match LocalListener::bind(endpoint, probe_timeout).await {
        Ok(listener) => {
            let (incoming, ownership) = listener.into_parts();
            Ok(GuiInstance::First(ActivationListener {
                incoming,
                _ownership: ownership,
            }))
        }
        // The probe that found the owner already asked it for the window; asking again is
        // harmless and doesn't depend on how the probe works.
        Err(BindUdsError::AlreadyOwned { .. }) => {
            tokio::time::timeout(probe_timeout, UnixStream::connect(endpoint.path()))
                .await
                .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
            Ok(GuiInstance::AlreadyRunning)
        }
        Err(error) => Err(io::Error::other(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    fn first(instance: GuiInstance) -> Option<ActivationListener> {
        match instance {
            GuiInstance::First(listener) => Some(listener),
            GuiInstance::AlreadyRunning => None,
        }
    }

    fn runtime_directory() -> tempfile::TempDir {
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap()
    }

    #[tokio::test]
    async fn a_later_launch_asks_the_first_for_its_window() {
        let directory = runtime_directory();
        let endpoint = LocalEndpoint::gui_from_root(directory.path()).unwrap();
        let mut running = first(claim(&endpoint, TEST_TIMEOUT).await.unwrap()).unwrap();

        let later = claim(&endpoint, TEST_TIMEOUT).await.unwrap();

        assert!(matches!(later, GuiInstance::AlreadyRunning));
        tokio::time::timeout(TEST_TIMEOUT, running.next_activation())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn a_launch_after_the_first_quit_becomes_the_first() {
        let directory = runtime_directory();
        let endpoint = LocalEndpoint::gui_from_root(directory.path()).unwrap();
        drop(claim(&endpoint, TEST_TIMEOUT).await.unwrap());

        let next = claim(&endpoint, TEST_TIMEOUT).await.unwrap();

        assert!(matches!(next, GuiInstance::First(_)));
    }

    #[test]
    fn the_gui_socket_sits_next_to_the_server_socket() {
        let directory = runtime_directory();

        let gui = LocalEndpoint::gui_from_root(directory.path()).unwrap();
        let server = LocalEndpoint::from_root(directory.path()).unwrap();

        assert_eq!(gui.path().parent(), server.path().parent());
        assert_ne!(gui.path(), server.path());
    }
}
