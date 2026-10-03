//! Tufo's command-line process root; all server requests go through tufo-client.

mod command;
mod data;
mod doctor;
mod gui;
mod render;
mod server;
mod song_loops;

use std::{io::Write as _, process::ExitCode};

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
    match run(arguments.command, console).await {
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

async fn run(command: command::Command, console: render::Console) -> anyhow::Result<bool> {
    // `data` reports through stderr only, so it bypasses the stdout report every other command
    // prints below.
    let (output, success) = match command {
        command::Command::Doctor { json } => doctor::run(console, "doctor", json).await?,
        command::Command::Server { command } => server::run(command, console).await?,
        command::Command::Gui { command } => gui::run(command, console).await?,
        command::Command::Data { command } => return data::run(command).await,
        command::Command::SongLoops { command } => song_loops::run(command, console).await?,
    };
    match writeln!(std::io::stdout().lock(), "{output}") {
        Ok(()) => Ok(success),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(success),
        Err(error) => Err(error.into()),
    }
}
