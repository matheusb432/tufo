use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum BrowserCommand {
    /// Read whether the browser runs, its volume and mute, and whether a page plays.
    State,
    /// Start the browser, restoring its last tabs; the GUI shows its pages.
    Open,
    /// Save the browser's tabs and end it with every page process.
    Close,
}

pub(crate) async fn run_browser(command: BrowserCommand, json: bool) -> anyhow::Result<bool> {
    let client = crate::api::connect().await?;
    match command {
        BrowserCommand::State => {
            let state = crate::api::player_state(&client).await?;
            crate::api::emit(&crate::record::browser(state.browser), json).await?;
        }
        BrowserCommand::Open => {
            client.open_browser().await?;
            crate::api::ack("open", "browser", "current", json).await?;
        }
        BrowserCommand::Close => {
            client.close_browser().await?;
            crate::api::ack("close", "browser", "current", json).await?;
        }
    }
    Ok(true)
}
