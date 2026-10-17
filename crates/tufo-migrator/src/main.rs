//! Authors the migration catalogue from the registered Toasty models.
//!
//! `tufo-server` applies the catalogue at startup; this tool only writes it.

mod migration_name;

use std::path::Path;

use anyhow::{Context as _, ensure};
use clap::{Parser, Subcommand};
use migration_name::MigrationName;
use toasty_cli::{Config, ToastyCli};

#[derive(Debug, Parser)]
#[command(
    name = "tufo-migrator",
    version,
    about = "Tufo database migration authoring",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate a migration from the difference between the models and the catalogue.
    Generate {
        /// Lowercase `snake_case` migration intent.
        name: MigrationName,
    },
    /// Print the registered Toasty model snapshot.
    Snapshot,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let arguments = match Cli::parse().command {
        Command::Generate { name } => vec![
            "tufo-migrator".to_owned(),
            "migration".to_owned(),
            "generate".to_owned(),
            "--name".to_owned(),
            name.as_str().to_owned(),
        ],
        Command::Snapshot => vec![
            "tufo-migrator".to_owned(),
            "migration".to_owned(),
            "snapshot".to_owned(),
        ],
    };
    let database = tufo_infra::database::build_authoring_database().await?;
    ToastyCli::with_config(database, load_config()?)
        .parse_from(arguments)
        .await
        .context("running the Toasty migration command")
}

/// Loads `Toasty.toml` and resolves its catalogue path against the file's own directory, so the
/// tool works from any working directory.
fn load_config() -> anyhow::Result<Config> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Toasty.toml");
    let mut config = Config::load_from(&path)?;
    let catalogue_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(&config.migration.path)
        .canonicalize()
        .with_context(|| {
            format!(
                "resolving the migration catalogue {}",
                config.migration.path.display()
            )
        })?;
    ensure!(
        catalogue_path.is_dir(),
        "the migration catalogue is not a directory: {}",
        catalogue_path.display()
    );
    config.migration.path = catalogue_path;
    Ok(config)
}
