//! The Windows named-pipe transport. The pipe's name carries the current user's SID, and its DACL
//! lets only that user connect.

use std::{
    io,
    os::windows::ffi::OsStrExt as _,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer},
};
use tonic::transport::{Channel, Endpoint, Uri, server::Connected};

use crate::ConnectError;

pub mod gui_instance;
mod security;

const PIPE_BUSY_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const GUI_PIPE_SUFFIX: &str = "-gui";
const BROWSER_PIPE_SUFFIX: &str = "-browser";

/// A named pipe only the current user can reach: the local `tufo-server`'s, the running GUI's, or
/// the running browser host's.
#[derive(Debug, Clone)]
pub struct LocalEndpoint {
    name: String,
    user_sid: String,
}

impl LocalEndpoint {
    /// Names the server's pipe after the current user, or after `TUFO_RUNTIME_DIR` when it is
    /// set, so isolated server instances never share a pipe.
    pub fn from_environment() -> io::Result<Self> {
        Self::from_environment_with_suffix("")
    }

    /// Uses an absolute runtime directory as the server's pipe namespace, without creating any
    /// files.
    pub fn from_root(root: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_root_with_suffix(root.as_ref(), "")
    }

    /// The running GUI's pipe, named like the server's.
    pub fn gui_from_environment() -> io::Result<Self> {
        Self::from_environment_with_suffix(GUI_PIPE_SUFFIX)
    }

    pub fn gui_from_root(root: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_root_with_suffix(root.as_ref(), GUI_PIPE_SUFFIX)
    }

    /// The running browser host's pipe, named like the server's.
    pub fn browser_from_environment() -> io::Result<Self> {
        Self::from_environment_with_suffix(BROWSER_PIPE_SUFFIX)
    }

    pub fn browser_from_root(root: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_root_with_suffix(root.as_ref(), BROWSER_PIPE_SUFFIX)
    }

    fn from_environment_with_suffix(suffix: &str) -> io::Result<Self> {
        if let Some(root) = std::env::var_os("TUFO_RUNTIME_DIR") {
            return Self::from_root_with_suffix(Path::new(&root), suffix);
        }
        let user_sid = security::current_user_sid()?;
        Ok(Self {
            name: format!(r"\\.\pipe\tufo-{user_sid}{suffix}"),
            user_sid,
        })
    }

    fn from_root_with_suffix(root: &Path, suffix: &str) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TUFO_RUNTIME_DIR must be absolute",
            ));
        }
        let namespace = fnv1a_64(root.as_os_str().encode_wide().flat_map(u16::to_le_bytes));
        let user_sid = security::current_user_sid()?;
        Ok(Self {
            name: format!(r"\\.\pipe\tufo-{user_sid}-{namespace:016x}{suffix}"),
            user_sid,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        Path::new(&self.name)
    }

    /// Checks transport availability without starting a server. Busy or inaccessible pipes
    /// return an error rather than claiming that the server stopped.
    pub async fn is_listening(&self, timeout: Duration) -> io::Result<bool> {
        let name = self.name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        // Zero and u32::MAX have special unbounded/default meanings in WaitNamedPipeW.
        let timeout_ms = u32::try_from(timeout.as_millis())
            .unwrap_or(u32::MAX - 1)
            .clamp(1, u32::MAX - 1);
        tokio::task::spawn_blocking(move || {
            // SAFETY: name is a live, NUL-terminated UTF-16 pipe name. This only waits for
            // availability; a probe never consumes or abruptly disconnects a pipe instance.
            if unsafe {
                windows_sys::Win32::System::Pipes::WaitNamedPipeW(name.as_ptr(), timeout_ms)
            } != 0
            {
                return Ok(true);
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(error)
            }
        })
        .await
        .map_err(io::Error::other)?
    }

    /// Connects eagerly, failing when no server listens on the pipe.
    pub async fn connect(
        &self,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Channel, ConnectError> {
        Ok(Self::endpoint(connect_timeout, request_timeout)
            .connect_with_connector(self.connector(connect_timeout))
            .await?)
    }

    /// Returns a channel that connects on first use and reconnects after the server restarts.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "matches the Unix transport, whose directory check can fail"
    )]
    pub fn connect_lazy(
        &self,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Channel, ConnectError> {
        Ok(Self::endpoint(connect_timeout, request_timeout)
            .connect_with_connector_lazy(self.connector(connect_timeout)))
    }

    fn endpoint(connect_timeout: Duration, request_timeout: Duration) -> Endpoint {
        // Tonic needs a URI to build requests; the connector ignores it and opens the pipe.
        Endpoint::from_static("http://tufo.local")
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
    }

    fn connector(
        &self,
        connect_timeout: Duration,
    ) -> impl tower::Service<
        Uri,
        Response = hyper_util::rt::TokioIo<NamedPipeClient>,
        Error = io::Error,
        Future = impl Future<Output = io::Result<hyper_util::rt::TokioIo<NamedPipeClient>>>
                 + Send
                 + 'static,
    > + Clone
    + Send
    + 'static {
        let name = self.name.clone();
        tower::service_fn(move |_: Uri| {
            let name = name.clone();
            async move {
                open_pipe(&name, connect_timeout)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        })
    }
}

/// A stable 64-bit FNV-1a hash, so the server and its clients derive the same pipe name from the
/// same runtime directory.
fn fnv1a_64(bytes: impl IntoIterator<Item = u8>) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;
    bytes.into_iter().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

/// Opens the pipe, waiting while every server instance is busy, for at most `timeout`.
async fn open_pipe(name: &str, timeout: Duration) -> io::Result<NamedPipeClient> {
    tokio::time::timeout(timeout, async {
        loop {
            match ClientOptions::new().open(name) {
                Ok(pipe) => return Ok(pipe),
                Err(error)
                    if error.raw_os_error()
                        == Some(windows_sys::Win32::Foundation::ERROR_PIPE_BUSY.cast_signed()) =>
                {
                    tokio::time::sleep(PIPE_BUSY_RETRY_INTERVAL).await;
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the tufo-server pipe stayed busy"))?
}

/// The first instance of the server's pipe. Binding fails while another server owns the name.
#[derive(Debug)]
pub struct LocalListener {
    endpoint: LocalEndpoint,
    server: NamedPipeServer,
}

impl LocalListener {
    /// Creates the first pipe instance. Windows removes a pipe when its last handle closes, so no
    /// stale pipe can outlive a crashed server and `_probe_timeout` goes unused. The result is a
    /// ready future so callers await it as they await the Unix transport's socket probe.
    pub fn bind(
        endpoint: &LocalEndpoint,
        _probe_timeout: Duration,
    ) -> std::future::Ready<io::Result<Self>> {
        std::future::ready(
            security::create_pipe(&endpoint.name, &endpoint.user_sid, true).map(|server| Self {
                endpoint: endpoint.clone(),
                server,
            }),
        )
    }

    /// Splits the listener into its stream of connections and a guard that has nothing to clean
    /// up on Windows.
    pub fn into_parts(
        self,
    ) -> (
        impl futures::Stream<Item = io::Result<PipeConnection>> + Send,
        (),
    ) {
        let incoming = futures::stream::try_unfold(self, |listener| async move {
            listener.server.connect().await?;
            // Keep a listening instance alive before handing this connection to Tonic.
            let next =
                security::create_pipe(&listener.endpoint.name, &listener.endpoint.user_sid, false)?;
            Ok(Some((
                PipeConnection(listener.server),
                Self {
                    endpoint: listener.endpoint,
                    server: next,
                },
            )))
        });
        (incoming, ())
    }
}

/// One client's connection to the server's pipe.
#[derive(Debug)]
pub struct PipeConnection(NamedPipeServer);

impl Connected for PipeConnection {
    type ConnectInfo = ();

    fn connect_info(&self) {}
}

impl AsyncRead for PipeConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(context, buffer)
    }
}

impl AsyncWrite for PipeConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(context)
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt as _;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(1);

    #[test]
    fn runtime_roots_name_distinct_pipes() {
        let first = LocalEndpoint::from_root(r"C:\tufo\first").unwrap();
        let second = LocalEndpoint::from_root(r"C:\tufo\second").unwrap();

        assert_ne!(first.path(), second.path());
        assert!(LocalEndpoint::from_root("relative").is_err());
    }

    #[tokio::test]
    async fn one_server_owns_the_pipe_and_serves_several_clients() {
        let root = tempfile::tempdir().unwrap();
        let endpoint = LocalEndpoint::from_root(root.path()).unwrap();
        assert!(!endpoint.is_listening(TEST_TIMEOUT).await.unwrap());
        let listener = LocalListener::bind(&endpoint, TEST_TIMEOUT).await.unwrap();
        assert!(LocalListener::bind(&endpoint, TEST_TIMEOUT).await.is_err());
        let (incoming, ()) = listener.into_parts();
        let mut incoming = Box::pin(incoming);
        for _ in 0..3 {
            let (client, server) = tokio::join!(
                async {
                    assert!(endpoint.is_listening(TEST_TIMEOUT).await.unwrap());
                    open_pipe(&endpoint.name, TEST_TIMEOUT).await.unwrap()
                },
                incoming.next(),
            );
            let mut client = client;
            let mut server = server.unwrap().unwrap();
            client.write_all(b"tufo").await.unwrap();
            let mut bytes = [0; 4];
            server.read_exact(&mut bytes).await.unwrap();
            assert_eq!(&bytes, b"tufo");
        }
        drop(incoming);
        // Mio releases cancelled overlapped I/O handles when the runtime processes completions.
        let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
        let mut rebound = LocalListener::bind(&endpoint, TEST_TIMEOUT).await;
        while rebound
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::PermissionDenied)
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "pipe handles did not close"
            );
            tokio::time::sleep(PIPE_BUSY_RETRY_INTERVAL).await;
            rebound = LocalListener::bind(&endpoint, TEST_TIMEOUT).await;
        }
        rebound.unwrap();
    }

    #[tokio::test]
    async fn waiting_for_a_busy_pipe_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        let endpoint = LocalEndpoint::from_root(root.path()).unwrap();
        let _listener = LocalListener::bind(&endpoint, TEST_TIMEOUT).await.unwrap();
        let _first = open_pipe(&endpoint.name, TEST_TIMEOUT).await.unwrap();

        let error = open_pipe(&endpoint.name, Duration::from_millis(30))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let error = endpoint
            .is_listening(Duration::from_millis(30))
            .await
            .unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            Some(windows_sys::Win32::Foundation::ERROR_SEM_TIMEOUT.cast_signed())
        );
    }
}
