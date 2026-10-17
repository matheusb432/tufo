//! Command-line process root; all server requests go through tufo-client.

macro_rules! releases_url {
    () => {
        concat!(env!("CARGO_PKG_REPOSITORY"), "/releases")
    };
}

mod api;
mod command;
mod data;
mod desktop_command;
mod doctor;
mod gui;
mod input;
mod kenku_command;
mod library_command;
mod library_values;
mod loop_command;
mod player_command;
mod record;
mod render;
mod server;
mod song_loops;

use std::process::ExitCode;

use clap::{CommandFactory as _, FromArgMatches as _, error::ErrorKind};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let console = render::Console::from_terminal();
    let mut parser = command::Cli::command().color(console.color_choice());
    let matches = parser.get_matches_mut();
    let arguments = command::Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    // Clap builds each subcommand's full usage while parsing, so runtime errors retain its scope.
    let mut context = &parser;
    let mut scope = &matches;
    while let Some((name, arguments)) = scope.subcommand() {
        context = match context.find_subcommand(name) {
            Some(command) => command,
            None => break,
        };
        scope = arguments;
    }
    match run(arguments.command, arguments.json, console).await {
        Ok(success) => {
            if success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            let _ = context
                .clone()
                .error(ErrorKind::Io, format!("{error:#}"))
                .print();
            ExitCode::FAILURE
        }
    }
}

async fn run(
    command: command::Command,
    json: bool,
    console: render::Console,
) -> anyhow::Result<bool> {
    // `data` reports through stderr only, so it bypasses the stdout report every other command
    // prints below.
    let (output, success) = match command {
        command::Command::Doctor => doctor::run(console, "doctor", json).await?,
        command::Command::Server { command } => server::run(command, console, json).await?,
        command::Command::Gui { command } => gui::run(command, console, json).await?,
        command::Command::Data { command } => return data::run(command).await,
        command::Command::SongLoops { command } => song_loops::run(command, console, json).await?,
        command::Command::Library { command } => {
            return library_command::run_library(command, json).await;
        }
        command::Command::Collection { command } => {
            return library_command::run_collection(command, json).await;
        }
        command::Command::Entry { command } => {
            return library_command::run_entry(command, json).await;
        }
        command::Command::Song { command } => {
            return library_command::run_song(command, json).await;
        }
        command::Command::Folder { command } => {
            return library_command::run_folder(command, json).await;
        }
        command::Command::Player { command } => {
            return player_command::run_player(command, json).await;
        }
        command::Command::Music { command } => {
            return player_command::run_music(command, json).await;
        }
        command::Command::Queue { command } => {
            return player_command::run_queue(command, json).await;
        }
        command::Command::Layer { command } => {
            return player_command::run_layer(command, json).await;
        }
        command::Command::Effect { command } => {
            return player_command::run_effect(command, json).await;
        }
        command::Command::Output { command } => {
            return player_command::run_output(command, json).await;
        }
        command::Command::Discord { command } => {
            return player_command::run_discord(command, json).await;
        }
        command::Command::Browser { command } => {
            return player_command::run_browser(command, json).await;
        }
        command::Command::Loop { command } => {
            return loop_command::run_loop(command, json).await;
        }
        command::Command::Kenku { command } => {
            return kenku_command::run_kenku(command, json).await;
        }
        command::Command::Desktop { command } => {
            return desktop_command::run(command, json).await;
        }
    };
    api::write_text(output).await?;
    Ok(success)
}
