use std::{
    fs::{File, OpenOptions},
    path::PathBuf,
    sync::Mutex,
};

use clap::{Parser, Subcommand};
use tracing_subscriber::fmt::writer::MakeWriterExt as _;

/// glibc's initial mmap threshold, which it would otherwise raise up to 32 MB.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const MMAP_THRESHOLD_BYTES: libc::c_int = 128 * 1024;
/// A log past this size moves aside to `tufo-server.log.1` when the server starts.
const LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;
const LOG_FILE_NAME: &str = "tufo-server.log";

/// Tufo's local server: the audio engine, the Discord bot, and the RPCs the GUI drives.
#[derive(Parser)]
#[command(version, about)]
struct Arguments {
    /// TOML file holding the Discord `bot_token`, readable only by you. Defaults to `discord.toml`
    /// in Tufo's config directory when that file exists.
    #[arg(long, value_name = "PATH", env = "TUFO_DISCORD_CREDENTIALS")]
    discord_credentials: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(
        subcommand,
        about = "Run a one-shot data snapshot operation for `tufo data`."
    )]
    Data(tufo_server::data::DataCommand),
}

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    configure_pipewire_client();
    tokio::runtime::Runtime::new()?.block_on(run())
}

#[cfg(target_os = "linux")]
fn configure_pipewire_client() {
    if std::env::var_os("PIPEWIRE_CONFIG_NAME").is_none() {
        // SAFETY: main calls this before starting the runtime or any other threads.
        unsafe { std::env::set_var("PIPEWIRE_CONFIG_NAME", "client-rt.conf") };
    }
}

async fn run() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    if let Some(Command::Data(command)) = arguments.command {
        return tufo_server::data::run(command).await;
    }
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // The GUI starts the server without a terminal, so the log also goes to a file.
    match open_log_file() {
        Some((file, path)) => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr.and(Mutex::new(file)))
                .init();
            tracing::info!(path = %path.display(), "logging to a file");
        }
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    map_large_allocations_separately();
    tufo_server::run(arguments.discord_credentials).await
}

/// After freeing a large block, glibc serves blocks of that size from heaps it rarely shrinks, so
/// decoded sounds and decode buffers freed while idle stayed resident. Mapping each large block
/// separately returns it to the system when freed.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn map_large_allocations_separately() {
    // SAFETY: mallopt only tunes the allocator and is safe to call at any time.
    let applied = unsafe { libc::mallopt(libc::M_MMAP_THRESHOLD, MMAP_THRESHOLD_BYTES) };
    if applied == 0 {
        tracing::warn!("glibc refused a fixed mmap threshold; freed memory may stay resident");
    }
}

/// Opens the log in Tufo's state directory, or its local data directory where the platform has
/// no state directory, rotating a log that grew too large.
fn open_log_file() -> Option<(File, PathBuf)> {
    let directories = directories::ProjectDirs::from("", "", "tufo")?;
    let folder = directories
        .state_dir()
        .unwrap_or_else(|| directories.data_local_dir())
        .to_path_buf();
    std::fs::create_dir_all(&folder).ok()?;
    let path = folder.join(LOG_FILE_NAME);
    let too_large =
        std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > LOG_ROTATE_BYTES);
    if too_large {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    Some((file, path))
}
