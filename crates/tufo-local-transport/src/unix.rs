//! The Unix socket transport, inside a directory only the current user may use.

use std::{
    io,
    os::unix::fs::{DirBuilderExt as _, MetadataExt as _},
    path::{Path, PathBuf},
    time::Duration,
};

use tonic::transport::{Channel, Endpoint};

use crate::ConnectError;

pub mod gui_instance;
pub mod listener;

#[cfg(test)]
use listener::LocalListener;

const SOCKET_FILE_NAME: &str = "tufo.sock";
const GUI_SOCKET_FILE_NAME: &str = "tufo-gui.sock";
const BROWSER_SOCKET_FILE_NAME: &str = "tufo-browser.sock";

/// A socket path inside a directory only the current user may use: the local `tufo-server`'s, the
/// running GUI's, or the running browser host's.
#[derive(Debug, Clone)]
pub struct LocalEndpoint {
    root: PathBuf,
    path: String,
}

impl LocalEndpoint {
    /// The server's socket in the runtime directory from `TUFO_RUNTIME_DIR`, falling back to
    /// `$XDG_RUNTIME_DIR/tufo`.
    pub fn from_environment() -> io::Result<Self> {
        Self::from_root(runtime_root_from_environment())
    }

    /// The server's socket in an absolute runtime directory, within the platform socket-path
    /// capacity.
    pub fn from_root(root: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_root_and_file_name(root.as_ref(), SOCKET_FILE_NAME)
    }

    /// The running GUI's socket, next to the server's.
    pub fn gui_from_environment() -> io::Result<Self> {
        Self::gui_from_root(runtime_root_from_environment())
    }

    pub fn gui_from_root(root: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_root_and_file_name(root.as_ref(), GUI_SOCKET_FILE_NAME)
    }

    /// The running browser host's socket, next to the server's.
    pub fn browser_from_environment() -> io::Result<Self> {
        Self::browser_from_root(runtime_root_from_environment())
    }

    pub fn browser_from_root(root: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_root_and_file_name(root.as_ref(), BROWSER_SOCKET_FILE_NAME)
    }

    fn from_root_and_file_name(root: &Path, file_name: &str) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TUFO_RUNTIME_DIR must be absolute",
            ));
        }
        let path = root
            .join(file_name)
            .into_os_string()
            .into_string()
            .map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "socket path must be UTF-8")
            })?;
        // SAFETY: a zeroed sockaddr_un contains only integer fields and a byte array.
        let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        if path.as_bytes().contains(&0) || path.len() >= address.sun_path.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "socket path contains NUL or exceeds the platform capacity",
            ));
        }
        Ok(Self {
            root: root.to_path_buf(),
            path,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        Path::new(&self.path)
    }

    /// Checks transport availability without creating a runtime directory or starting a server.
    /// Permission errors and timeouts are errors, not evidence that the server stopped.
    pub async fn is_listening(&self, timeout: Duration) -> io::Result<bool> {
        match inspect_directory(&self.root) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        }
        match tokio::time::timeout(timeout, tokio::net::UnixStream::connect(self.path())).await {
            Ok(connect) => Ok(ListenerAnswer::from_connect(connect)? == ListenerAnswer::Listening),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the tufo-server socket did not answer",
            )),
        }
    }

    /// Connects eagerly, failing when no server listens on the socket.
    pub async fn connect(
        &self,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Channel, ConnectError> {
        Ok(self
            .endpoint(connect_timeout, request_timeout)?
            .connect()
            .await?)
    }

    /// Returns a channel that connects on first use and reconnects after the server restarts.
    pub fn connect_lazy(
        &self,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Channel, ConnectError> {
        Ok(self
            .endpoint(connect_timeout, request_timeout)?
            .connect_lazy())
    }

    fn endpoint(
        &self,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Endpoint, ConnectError> {
        inspect_directory(&self.root)?;
        Ok(Endpoint::from_shared(format!("unix:{}", self.path))?
            .connect_timeout(connect_timeout)
            .timeout(request_timeout))
    }

    fn prepare_directory(&self) -> io::Result<()> {
        match std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)
        {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        inspect_directory(&self.root)
    }
}

/// What a connect attempt shows about the server behind a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerAnswer {
    Listening,
    NotListening,
}

impl ListenerAnswer {
    /// Errors that don't show whether a server listens, such as permission errors, stay errors.
    fn from_connect<T>(connect: io::Result<T>) -> io::Result<Self> {
        match connect {
            Ok(_) => Ok(Self::Listening),
            // A Unix connect completes once the listener's backlog queues it. A listener that
            // closes before accepting resets each queued connection, and tokio's connect returns
            // that reset.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound
                        | io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::ConnectionReset
                ) =>
            {
                Ok(Self::NotListening)
            }
            Err(error) => Err(error),
        }
    }
}

fn runtime_root_from_environment() -> PathBuf {
    std::env::var_os("TUFO_RUNTIME_DIR").map_or_else(default_runtime_root, PathBuf::from)
}

fn default_runtime_root() -> PathBuf {
    directories::ProjectDirs::from("", "", "tufo")
        .and_then(|directories| directories.runtime_dir().map(Path::to_path_buf))
        .unwrap_or_else(|| std::env::temp_dir().join(format!("tufo-{}", effective_user_id())))
}

fn effective_user_id() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

fn inspect_directory(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != effective_user_id()
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "runtime directory must be owned by the current user with mode 0700: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn invalid_paths_do_not_create_directories() {
        assert!(LocalEndpoint::from_root("relative").is_err());
        let directory = tempfile::tempdir().unwrap();
        let long = directory.path().join("x".repeat(256));
        assert!(LocalEndpoint::from_root(&long).is_err());
        assert!(!long.exists());
        assert!(LocalEndpoint::from_root(directory.path().join("nul\0")).is_err());
    }

    #[test]
    fn connect_results_answer_whether_a_server_listens() {
        use ListenerAnswer::{Listening, NotListening};
        use io::ErrorKind::{ConnectionRefused, ConnectionReset, NotFound, PermissionDenied};

        assert_eq!(ListenerAnswer::from_connect(Ok(())).unwrap(), Listening);
        let error_answers = [
            (NotFound, Ok(NotListening)),
            (ConnectionRefused, Ok(NotListening)),
            (ConnectionReset, Ok(NotListening)),
            (PermissionDenied, Err(PermissionDenied)),
        ];
        for (kind, expected) in error_answers {
            let answer = ListenerAnswer::from_connect(Err::<(), _>(io::Error::from(kind)))
                .map_err(|error| error.kind());
            assert_eq!(answer, expected, "{kind:?}");
        }
    }

    #[tokio::test]
    async fn listener_rejects_unsafe_or_symlinked_parent() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("runtime");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let endpoint = LocalEndpoint::from_root(&root).unwrap();
        assert!(
            LocalListener::bind(&endpoint, Duration::from_millis(100))
                .await
                .is_err()
        );
        std::fs::remove_dir(&root).unwrap();
        std::os::unix::fs::symlink(directory.path(), &root).unwrap();
        assert!(
            LocalListener::bind(&endpoint, Duration::from_millis(100))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn listener_rejects_non_socket_and_symlink() {
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let endpoint = LocalEndpoint::from_root(directory.path()).unwrap();
        std::fs::write(endpoint.path(), "keep").unwrap();
        assert!(
            LocalListener::bind(&endpoint, Duration::from_millis(100))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(endpoint.path()).unwrap(), "keep");
        std::fs::remove_file(endpoint.path()).unwrap();
        let target = directory.path().join("target");
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, endpoint.path()).unwrap();
        assert!(
            LocalListener::bind(&endpoint, Duration::from_millis(100))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(target).unwrap(), "keep");
    }
}
