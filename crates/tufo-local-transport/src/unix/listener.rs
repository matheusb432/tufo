//! Binds a private socket and recovers one left behind by a crashed process.

use std::{
    io,
    os::unix::fs::{FileTypeExt as _, MetadataExt as _},
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::net::{UnixListener, UnixStream};
use tokio_stream::wrappers::UnixListenerStream;

use super::{ListenerAnswer, LocalEndpoint};

const PRIVATE_SOCKET_MODE: u32 = 0o600;

#[derive(Debug, thiserror::Error)]
pub enum BindUdsError {
    #[error("cannot prepare the private socket directory")]
    Directory {
        #[source]
        source: io::Error,
    },
    #[error("the local socket path is a symbolic link: {}", path.display())]
    SymbolicLink { path: PathBuf },
    #[error("the local socket path is not a socket: {}", path.display())]
    NotSocket { path: PathBuf },
    #[error("the local socket has unsafe ownership or permissions: {}", path.display())]
    UnsafeSocket { path: PathBuf },
    #[error("another process owns the local socket: {}", path.display())]
    AlreadyOwned { path: PathBuf },
    #[error("timed out while probing the existing local socket: {}", path.display())]
    ProbeTimedOut { path: PathBuf },
    #[error("the local socket changed while it was being checked: {}", path.display())]
    ChangedDuringProbe { path: PathBuf },
    #[error("failed to inspect the local socket: {}", path.display())]
    Inspect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to probe the existing local socket: {}", path.display())]
    Probe {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to remove the stale local socket: {}", path.display())]
    RemoveStale {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to bind the local socket: {}", path.display())]
    Bind {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to set private permissions on the local socket: {}", path.display())]
    SetPermissions {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the bound local socket was replaced before startup completed: {}", path.display())]
    BoundSocketReplaced { path: PathBuf },
}

#[derive(Debug)]
pub struct LocalListener {
    listener: UnixListener,
    ownership: OwnedSocketPath,
}

impl LocalListener {
    pub async fn bind(
        endpoint: &LocalEndpoint,
        probe_timeout: Duration,
    ) -> Result<Self, BindUdsError> {
        endpoint
            .prepare_directory()
            .map_err(|source| BindUdsError::Directory { source })?;
        let path = endpoint.path();
        prepare_path(path, probe_timeout).await?;

        let listener = UnixListener::bind(path).map_err(|source| BindUdsError::Bind {
            path: path.to_path_buf(),
            source,
        })?;
        let identity = inspect_socket(path, false)?.identity;
        let ownership = OwnedSocketPath {
            path: path.to_path_buf(),
            identity,
        };

        set_private_permissions(path).map_err(|source| BindUdsError::SetPermissions {
            path: path.to_path_buf(),
            source,
        })?;
        let metadata = inspect_socket(path, true)?;
        if metadata.identity != identity || metadata.mode != PRIVATE_SOCKET_MODE {
            return Err(BindUdsError::BoundSocketReplaced {
                path: path.to_path_buf(),
            });
        }

        Ok(Self {
            listener,
            ownership,
        })
    }

    pub fn into_parts(self) -> (UnixListenerStream, OwnedSocketPath) {
        (UnixListenerStream::new(self.listener), self.ownership)
    }
}

#[derive(Debug)]
pub struct OwnedSocketPath {
    path: PathBuf,
    identity: SocketIdentity,
}

impl Drop for OwnedSocketPath {
    fn drop(&mut self) {
        remove_if_same_socket(&self.path, self.identity);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug)]
struct ExistingSocket {
    identity: SocketIdentity,
    mode: u32,
}

async fn prepare_path(path: &Path, probe_timeout: Duration) -> Result<(), BindUdsError> {
    let existing = match inspect_existing_socket(path) {
        Ok(existing) => existing,
        Err(BindUdsError::Inspect { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    let connect = tokio::time::timeout(probe_timeout, UnixStream::connect(path)).await;
    match connect.map(ListenerAnswer::from_connect) {
        Ok(Ok(ListenerAnswer::Listening)) => Err(BindUdsError::AlreadyOwned {
            path: path.to_path_buf(),
        }),
        Ok(Ok(ListenerAnswer::NotListening)) => remove_stale_if_unchanged(path, existing.identity),
        Ok(Err(source)) => Err(BindUdsError::Probe {
            path: path.to_path_buf(),
            source,
        }),
        Err(_) => Err(BindUdsError::ProbeTimedOut {
            path: path.to_path_buf(),
        }),
    }
}

fn inspect_existing_socket(path: &Path) -> Result<ExistingSocket, BindUdsError> {
    inspect_socket(path, true)
}

fn inspect_socket(path: &Path, require_private_mode: bool) -> Result<ExistingSocket, BindUdsError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| BindUdsError::Inspect {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(BindUdsError::SymbolicLink {
            path: path.to_path_buf(),
        });
    }
    if !metadata.file_type().is_socket() {
        return Err(BindUdsError::NotSocket {
            path: path.to_path_buf(),
        });
    }
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_user_id = unsafe { libc::geteuid() };
    let mode = metadata.mode() & 0o777;
    if metadata.uid() != effective_user_id
        || require_private_mode && mode != PRIVATE_SOCKET_MODE
        || metadata.nlink() != 1
    {
        return Err(BindUdsError::UnsafeSocket {
            path: path.to_path_buf(),
        });
    }
    Ok(ExistingSocket {
        identity: SocketIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        mode,
    })
}

fn remove_stale_if_unchanged(
    path: &Path,
    expected_identity: SocketIdentity,
) -> Result<(), BindUdsError> {
    let current = match inspect_existing_socket(path) {
        Ok(current) => current,
        Err(BindUdsError::Inspect { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if current.identity != expected_identity {
        return Err(BindUdsError::ChangedDuringProbe {
            path: path.to_path_buf(),
        });
    }
    std::fs::remove_file(path).map_err(|source| BindUdsError::RemoveStale {
        path: path.to_path_buf(),
        source,
    })
}

fn remove_if_same_socket(path: &Path, expected_identity: SocketIdentity) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.file_type().is_socket()
        || metadata.dev() != expected_identity.device
        || metadata.ino() != expected_identity.inode
    {
        return;
    }
    let _ = std::fs::remove_file(path);
}

fn set_private_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_SOCKET_MODE))
}

#[cfg(test)]
mod tests {
    use std::os::unix::{fs::PermissionsExt as _, net};

    use super::*;

    const TEST_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

    fn socket_directory() -> tempfile::TempDir {
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap()
    }

    fn endpoint(path: &Path) -> LocalEndpoint {
        LocalEndpoint::from_root(path.parent().unwrap()).unwrap()
    }

    fn private_socket(path: &Path) -> net::UnixListener {
        let listener = net::UnixListener::bind(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_SOCKET_MODE))
            .unwrap();
        listener
    }

    #[tokio::test]
    async fn rejects_a_socket_owned_by_a_live_server() {
        let directory = socket_directory();
        let path = directory.path().join("tufo.sock");
        let _live_listener = private_socket(&path);

        let error = LocalListener::bind(&endpoint(&path), TEST_PROBE_TIMEOUT)
            .await
            .unwrap_err();

        assert!(matches!(error, BindUdsError::AlreadyOwned { .. }));
        assert!(path.exists());
    }

    #[tokio::test]
    async fn removes_a_stale_socket_before_binding() {
        let directory = socket_directory();
        let path = directory.path().join("tufo.sock");
        drop(private_socket(&path));

        let bound = LocalListener::bind(&endpoint(&path), TEST_PROBE_TIMEOUT)
            .await
            .unwrap();

        assert_eq!(
            std::fs::symlink_metadata(&path).unwrap().mode() & 0o777,
            PRIVATE_SOCKET_MODE
        );
        drop(bound);
        assert!(!path.exists());
    }

    #[test]
    fn stale_recheck_preserves_a_replacement_socket() {
        let directory = socket_directory();
        let path = directory.path().join("tufo.sock");
        drop(private_socket(&path));
        let stale_identity = inspect_existing_socket(&path).unwrap().identity;
        std::fs::rename(&path, directory.path().join("stale.sock")).unwrap();
        let _replacement = private_socket(&path);

        let error = remove_stale_if_unchanged(&path, stale_identity).unwrap_err();

        assert!(matches!(error, BindUdsError::ChangedDuringProbe { .. }));
        assert!(path.exists());
    }

    #[tokio::test]
    async fn cleanup_preserves_a_replacement_socket() {
        let directory = socket_directory();
        let path = directory.path().join("tufo.sock");
        let bound = LocalListener::bind(&endpoint(&path), TEST_PROBE_TIMEOUT)
            .await
            .unwrap();
        std::fs::rename(&path, directory.path().join("owned.sock")).unwrap();
        let _replacement = private_socket(&path);

        drop(bound);

        assert!(path.exists());
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn ambiguous_probe_preserves_the_socket() {
        use std::os::fd::AsRawFd as _;

        let directory = socket_directory();
        let path = directory.path().join("tufo.sock");
        let listener = private_socket(&path);
        // SAFETY: the owned descriptor names a listening Unix socket.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
        let _first = UnixStream::connect(&path).await.unwrap();
        let _second = UnixStream::connect(&path).await.unwrap();
        let identity = inspect_existing_socket(&path).unwrap().identity;
        let error = LocalListener::bind(&endpoint(&path), Duration::from_millis(20))
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                BindUdsError::ProbeTimedOut { .. } | BindUdsError::Probe { .. }
            ),
            "{error:?}"
        );
        assert_eq!(inspect_existing_socket(&path).unwrap().identity, identity);
    }
}
