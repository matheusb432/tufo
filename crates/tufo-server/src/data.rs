use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, ensure};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use tufo_infra::database::{
    self,
    snapshot::{self, DatabaseSchemaVersion},
};

const SNAPSHOT_FORMAT_VERSION: u32 = 1;
const SNAPSHOT_APPLICATION: &str = "tufo";
const SNAPSHOT_MANIFEST_FILE_NAME: &str = "manifest.json";
const SNAPSHOT_DATABASE_FILE_NAME: &str = "tufo.sqlite3";
const SNAPSHOT_SETTINGS_FILE_NAME: &str = "config.toml";
const STAGED_DATABASE_SUFFIX: &str = ".import";

#[derive(Subcommand)]
pub enum DataCommand {
    Export {
        #[arg(long, value_name = "DIR")]
        to: PathBuf,
    },
    StageImport {
        #[arg(long, value_name = "DIR")]
        from: PathBuf,
    },
    FinishImport {
        #[arg(long, value_name = "DIR")]
        from: PathBuf,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct DataSnapshotManifest {
    format_version: u32,
    application: String,
    schema_version: i64,
}

impl DataSnapshotManifest {
    fn new(schema_version: DatabaseSchemaVersion) -> Self {
        Self {
            format_version: SNAPSHOT_FORMAT_VERSION,
            application: SNAPSHOT_APPLICATION.to_owned(),
            schema_version: schema_version.get(),
        }
    }

    fn read(snapshot_directory: &Path) -> anyhow::Result<Self> {
        let path = snapshot_directory.join(SNAPSHOT_MANIFEST_FILE_NAME);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading snapshot manifest {}", path.display()))?;
        let manifest: Self = serde_json::from_str(&text)
            .with_context(|| format!("parsing snapshot manifest {}", path.display()))?;
        ensure!(
            manifest.application == SNAPSHOT_APPLICATION,
            "snapshot belongs to `{}`, not `{SNAPSHOT_APPLICATION}`",
            manifest.application
        );
        ensure!(
            manifest.format_version == SNAPSHOT_FORMAT_VERSION,
            "snapshot format version {} is unsupported; expected {SNAPSHOT_FORMAT_VERSION}",
            manifest.format_version
        );
        Ok(manifest)
    }
}

pub async fn run(command: DataCommand) -> anyhow::Result<()> {
    let database_path = database::database_path()?;
    match command {
        DataCommand::Export { to } => export(&database_path, &to),
        DataCommand::StageImport { from } => stage_import(&database_path, &from).await,
        DataCommand::FinishImport { from } => finish_import(&database_path, &from),
    }
}

fn export(database_path: &Path, snapshot_directory: &Path) -> anyhow::Result<()> {
    fs::create_dir(snapshot_directory).with_context(|| {
        format!(
            "creating snapshot directory {}",
            snapshot_directory.display()
        )
    })?;
    match write_snapshot(database_path, snapshot_directory) {
        Ok(schema_version) => {
            eprintln!(
                "exported database at schema version {} to {}",
                schema_version.get(),
                snapshot_directory.display()
            );
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_dir_all(snapshot_directory);
            Err(error)
        }
    }
}

fn write_snapshot(
    database_path: &Path,
    snapshot_directory: &Path,
) -> anyhow::Result<DatabaseSchemaVersion> {
    let schema_version = snapshot::export_database_snapshot(
        database_path,
        &snapshot_directory.join(SNAPSHOT_DATABASE_FILE_NAME),
    )?;
    copy_settings_file(snapshot_directory)?;
    let manifest = serde_json::to_string_pretty(&DataSnapshotManifest::new(schema_version))
        .context("serializing snapshot manifest")?;
    fs::write(
        snapshot_directory.join(SNAPSHOT_MANIFEST_FILE_NAME),
        manifest + "\n",
    )
    .context("writing snapshot manifest")?;
    Ok(schema_version)
}

async fn stage_import(database_path: &Path, snapshot_directory: &Path) -> anyhow::Result<()> {
    DataSnapshotManifest::read(snapshot_directory)?;
    let database_directory = database_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .with_context(|| format!("database path has no parent: {}", database_path.display()))?;
    fs::create_dir_all(database_directory).with_context(|| {
        format!(
            "creating database directory {}",
            database_directory.display()
        )
    })?;

    let staged = staged_database_path(database_path);
    snapshot::remove_staged_database(&staged)?;
    match snapshot::stage_database_snapshot(
        &snapshot_directory.join(SNAPSHOT_DATABASE_FILE_NAME),
        &staged,
    )
    .await
    {
        Ok(staged_snapshot) => {
            eprintln!(
                "staged database from {} and migrated schema version {} to {}",
                snapshot_directory.display(),
                staged_snapshot.snapshot_schema_version.get(),
                staged_snapshot.schema_version.get()
            );
            Ok(())
        }
        Err(error) => {
            let _ = snapshot::remove_staged_database(&staged);
            Err(error)
        }
    }
}

fn finish_import(database_path: &Path, snapshot_directory: &Path) -> anyhow::Result<()> {
    let staged = staged_database_path(database_path);
    ensure!(
        staged.is_file(),
        "no staged database import at {}",
        staged.display()
    );
    snapshot::replace_database_file(&staged, database_path)?;
    restore_settings_file(snapshot_directory)
}

fn staged_database_path(database_path: &Path) -> PathBuf {
    let mut path = database_path.as_os_str().to_os_string();
    path.push(STAGED_DATABASE_SUFFIX);
    path.into()
}

fn settings_path() -> Option<PathBuf> {
    let directories = directories::ProjectDirs::from("", "", "tufo")?;
    Some(directories.config_dir().join(SNAPSHOT_SETTINGS_FILE_NAME))
}

fn copy_settings_file(snapshot_directory: &Path) -> anyhow::Result<()> {
    let Some(source) = settings_path() else {
        return Ok(());
    };
    match fs::copy(
        &source,
        snapshot_directory.join(SNAPSHOT_SETTINGS_FILE_NAME),
    ) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("copying desktop settings {}", source.display()))
        }
    }
}

fn restore_settings_file(snapshot_directory: &Path) -> anyhow::Result<()> {
    let snapshot_settings = snapshot_directory.join(SNAPSHOT_SETTINGS_FILE_NAME);
    if !snapshot_settings.is_file() {
        return Ok(());
    }
    let destination = settings_path()
        .context("cannot resolve the desktop settings directory to restore config.toml")?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating desktop settings directory {}", parent.display()))?;
    }
    fs::copy(&snapshot_settings, &destination)
        .with_context(|| format!("restoring desktop settings to {}", destination.display()))?;
    Ok(())
}
