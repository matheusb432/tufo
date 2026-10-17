use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use cargo_metadata::{Metadata, MetadataCommand};
use clap::{Parser, Subcommand};

mod app_icon;
mod architecture;
mod cef_distribution;
mod install;
mod release;
mod translations;
mod windows_browser;
mod windows_installer;

#[derive(Parser)]
#[command(
    version,
    about = "Repository automation",
    styles = clap_cargo::style::CLAP_STYLING
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the workspace dependency graph against the layer policy.
    #[command(name = "check-architecture")]
    CheckArchitecture,
    /// Render the app icon's SVG into the Windows icon the GUI executable embeds.
    #[command(name = "app-icon")]
    AppIcon,
    /// Keep translation source references stable when source lines move.
    #[command(name = "translations-fmt")]
    TranslationsFmt {
        /// Check formatting without modifying the catalogs.
        #[arg(long)]
        check: bool,
    },
    /// Install the release build for the current user, with a launcher desktop search finds.
    Install,
    /// Close the session's GUI and its server, then install the release build over them.
    Update,
    /// Close the session's GUI and server, then remove what `install` placed, keeping the library,
    /// settings, and credentials.
    Uninstall,
    /// Cross-build the Windows executables and pack them into an unsigned installer in `dist/`.
    #[command(name = "windows-installer")]
    WindowsInstaller,
    /// Package the release build for this platform in `dist/`, with its checksum.
    Package {
        #[arg(value_enum)]
        platform: release::Platform,
    },
}

fn workspace_metadata(workspace_root: &Path) -> Result<Metadata> {
    MetadataCommand::new()
        .manifest_path(workspace_root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .context("read Cargo metadata")
}

/// Cargo's release output, which honors `CARGO_TARGET_DIR`.
fn release_dir(workspace_root: &Path) -> Result<PathBuf> {
    Ok(workspace_metadata(workspace_root)?
        .target_directory
        .join("release")
        .into_std_path_buf())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask must be one directory beneath the workspace root")?;

    match cli.command {
        Command::CheckArchitecture => architecture::check(workspace_root),
        Command::AppIcon => app_icon::render(workspace_root),
        Command::TranslationsFmt { check } => translations::format_catalogs(workspace_root, check),
        Command::Install => install::install(workspace_root, &release_dir(workspace_root)?),
        Command::Update => install::update(workspace_root, &release_dir(workspace_root)?),
        Command::Uninstall => install::uninstall(),
        Command::WindowsInstaller => {
            windows_installer::build(workspace_root, &workspace_metadata(workspace_root)?)
        }
        Command::Package { platform } => release::package(
            workspace_root,
            &workspace_metadata(workspace_root)?,
            platform,
        ),
    }
}
