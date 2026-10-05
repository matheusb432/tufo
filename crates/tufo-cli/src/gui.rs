use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use clap::Subcommand;
use tufo_client::process::Executable;

use crate::render::Console;

/// A GUI that finds its window already running raises that window and exits within this time;
/// one that fails to start exits within it too.
const STARTUP_EXIT_WAIT: Duration = Duration::from_secs(1);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Subcommand)]
pub(crate) enum GuiCommand {
    /// Start the sibling GUI (or tufo-gui on PATH), which raises the running window instead when
    /// there is one.
    Open,
}

pub(crate) async fn run(
    command: GuiCommand,
    console: Console,
    json: bool,
) -> anyhow::Result<(String, bool)> {
    let detail = match command {
        GuiCommand::Open => open().await?,
    };
    if json {
        return Ok((
            serde_json::to_string(&serde_json::json!({
                "action": "open",
                "resource": "gui",
                "id": "window",
                "status": detail,
            }))?,
            true,
        ));
    }
    Ok((
        format!(
            "{}\n{}",
            console.heading("gui"),
            console.row("OK", "Window", detail)
        ),
        true,
    ))
}

async fn open() -> anyhow::Result<&'static str> {
    let mut gui = Executable::Gui
        .command()
        .spawn()
        .context("start tufo-gui; install it beside tufo or put it on PATH")?;
    let deadline = Instant::now() + STARTUP_EXIT_WAIT;
    loop {
        if let Some(status) = gui.try_wait().context("check tufo-gui startup")? {
            ensure!(
                status.success(),
                "tufo-gui exited during startup ({status}); run tufo-gui in a terminal to see why"
            );
            return Ok("Already running; raised");
        }
        if Instant::now() >= deadline {
            // The GUI must outlive this CLI. Dropping Child does not terminate it.
            return Ok("Opened");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
