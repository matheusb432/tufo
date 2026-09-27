//! The `tufo-server` binary, reached through its local endpoint the way the GUI reaches it.

use std::{
    io::Read as _,
    process::{Child, Command, Stdio},
    time::Duration,
};

use anyhow::Context as _;
use tonic_health::{
    ServingStatus,
    pb::{HealthCheckRequest, health_client::HealthClient},
};
use tufo_local_transport::LocalEndpoint;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// Kills the server when the test ends, whether it passed or not.
struct ServerProcess(Child);

impl ServerProcess {
    /// Kills the server and returns what it wrote to stderr, for a failure message.
    fn kill_and_read_stderr(&mut self) -> String {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let mut stderr = String::new();
        if let Some(pipe) = self.0.stderr.as_mut() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        stderr
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn the_server_binary_answers_health_checks_on_its_local_endpoint() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let runtime = directory.path().join("runtime");
    let mut command = Command::new(env!("CARGO_BIN_EXE_tufo-server"));
    command
        .env("TUFO_RUNTIME_DIR", &runtime)
        .env("TUFO_DATABASE_PATH", directory.path().join("tufo.sqlite3"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // Keeps the server away from the developer's Discord credentials and Tufo data.
    #[cfg(unix)]
    for variable in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        command.env(variable, directory.path().join(variable));
    }
    let mut server = ServerProcess(command.spawn().context("starting tufo-server")?);
    let endpoint = LocalEndpoint::from_root(&runtime)?;

    let Ok(status) = tokio::time::timeout(STARTUP_TIMEOUT, serving_status(&endpoint)).await else {
        anyhow::bail!(
            "tufo-server did not answer within {STARTUP_TIMEOUT:?}; its stderr:\n{}",
            server.kill_and_read_stderr()
        );
    };

    assert_eq!(status, ServingStatus::Serving as i32);
    Ok(())
}

/// Retries until the server accepts a connection, then asks for its overall health.
async fn serving_status(endpoint: &LocalEndpoint) -> i32 {
    loop {
        if let Ok(channel) = endpoint
            .connect(CONNECT_ATTEMPT_TIMEOUT, CONNECT_ATTEMPT_TIMEOUT)
            .await
        {
            let request = HealthCheckRequest {
                service: String::new(),
            };
            if let Ok(response) = HealthClient::new(channel).check(request).await {
                return response.into_inner().status;
            }
        }
        tokio::time::sleep(CONNECT_RETRY_INTERVAL).await;
    }
}
