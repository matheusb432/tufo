use std::{ffi::OsStr, path::Path};

use anyhow::{Context as _, ensure};
use tufo_client::{LocalEndpoint, process::Executable};

use crate::{command::DataCommand, server};

pub(crate) async fn run(command: DataCommand) -> anyhow::Result<bool> {
    match command {
        DataCommand::Export { to } => {
            run_server_data([OsStr::new("export"), OsStr::new("--to"), to.as_os_str()])?;
        }
        DataCommand::Import { from } => import(&from).await?,
    }
    Ok(true)
}

async fn import(snapshot_directory: &Path) -> anyhow::Result<()> {
    let from = [OsStr::new("--from"), snapshot_directory.as_os_str()];
    run_server_data([OsStr::new("stage-import")].into_iter().chain(from))?;

    let endpoint = LocalEndpoint::from_environment().context("resolve TUFO_RUNTIME_DIR")?;
    let server_was_active = server::inspect(&endpoint).await?.is_some();
    if server_was_active {
        server::stop(&endpoint).await?;
    }
    run_server_data([OsStr::new("finish-import")].into_iter().chain(from))?;
    if server_was_active {
        server::start(&endpoint)
            .await
            .context("the data was imported, but the server did not start")?;
    }
    eprintln!("imported data from {}", snapshot_directory.display());
    Ok(())
}

fn run_server_data<'argument>(
    arguments: impl IntoIterator<Item = &'argument OsStr>,
) -> anyhow::Result<()> {
    let status = Executable::Server
        .attached_command()
        .arg("data")
        .args(arguments)
        .status()
        .context("run tufo-server data; install it beside tufo or put it on PATH")?;
    ensure!(status.success(), "tufo-server data failed with {status}");
    Ok(())
}
