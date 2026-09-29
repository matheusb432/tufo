use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, bail, ensure};
use rusqlite::{Connection, OpenFlags};

use super::{
    DATABASE_MIGRATION_CONNECTIONS_MAX, connect_database,
    driver::{ConfiguredSqlite, DatabaseConnectionRole},
    migrations,
};

const MIGRATION_LEDGER_TABLE: &str = "__toasty_migrations";
const SQLITE_COMPANION_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];
const LOCK_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DatabaseSchemaVersion(i64);

impl DatabaseSchemaVersion {
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct StagedDatabaseSnapshot {
    pub snapshot_schema_version: DatabaseSchemaVersion,
    pub schema_version: DatabaseSchemaVersion,
}

pub fn export_database_snapshot(
    database_path: &Path,
    snapshot_path: &Path,
) -> anyhow::Result<DatabaseSchemaVersion> {
    ensure!(
        database_path.is_file(),
        "database does not exist: {}",
        database_path.display()
    );
    let snapshot_path_text = snapshot_path.to_str().with_context(|| {
        format!(
            "snapshot path is not valid UTF-8: {}",
            snapshot_path.display()
        )
    })?;

    let live = Connection::open_with_flags(database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening database {}", database_path.display()))?;
    live.busy_timeout(LOCK_WAIT_MAX)
        .with_context(|| format!("configuring lock wait on {}", database_path.display()))?;
    live.execute("VACUUM INTO ?1", [snapshot_path_text])
        .with_context(|| format!("writing database snapshot to {}", snapshot_path.display()))?;
    drop(live);

    let snapshot = single_file_connection(snapshot_path)?;
    schema_version(&snapshot).with_context(|| {
        format!(
            "database snapshot has no applied migrations: {}",
            snapshot_path.display()
        )
    })
}

pub async fn stage_database_snapshot(
    snapshot_path: &Path,
    staged_path: &Path,
) -> anyhow::Result<StagedDatabaseSnapshot> {
    copy_file_new(snapshot_path, staged_path).with_context(|| {
        format!(
            "copying database snapshot {} to {}",
            snapshot_path.display(),
            staged_path.display()
        )
    })?;
    let snapshot_schema_version = schema_version(&single_file_connection(staged_path)?)
        .with_context(|| {
            format!(
                "database snapshot has no applied migrations: {}",
                staged_path.display()
            )
        })?;

    let driver = ConfiguredSqlite::new(staged_path, DatabaseConnectionRole::Migration)
        .context("configuring the staged database's migration driver")?;
    let migration_database =
        connect_database(staged_path, driver, DATABASE_MIGRATION_CONNECTIONS_MAX).await?;
    migrations::migrate_database(&migration_database)
        .await
        .context("forward-migrating the staged database snapshot")?;
    drop(migration_database);

    let staged = single_file_connection_once_unlocked(staged_path).await?;
    let report: String = staged
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .with_context(|| {
            format!(
                "checking staged database integrity {}",
                staged_path.display()
            )
        })?;
    ensure!(
        report == "ok",
        "staged database failed its integrity check: {report}"
    );

    Ok(StagedDatabaseSnapshot {
        snapshot_schema_version,
        schema_version: schema_version(&staged)?,
    })
}

pub fn replace_database_file(staged_path: &Path, database_path: &Path) -> anyhow::Result<()> {
    for suffix in SQLITE_COMPANION_SUFFIXES {
        let companion = companion_path(database_path, suffix);
        match fs::remove_file(&companion) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(source).with_context(|| {
                    format!("removing stale database companion {}", companion.display())
                });
            }
        }
    }
    fs::rename(staged_path, database_path).with_context(|| {
        format!(
            "moving staged database into place at {}",
            database_path.display()
        )
    })
}

fn single_file_connection(path: &Path) -> anyhow::Result<Connection> {
    let (connection, mode) = try_single_file_connection(path)
        .with_context(|| format!("opening database {}", path.display()))?;
    ensure!(
        mode.eq_ignore_ascii_case("delete"),
        "{} is still in journal mode {mode}; another connection may still be open",
        path.display()
    );
    Ok(connection)
}

async fn single_file_connection_once_unlocked(path: &Path) -> anyhow::Result<Connection> {
    let deadline = tokio::time::Instant::now() + LOCK_WAIT_MAX;
    loop {
        let outcome = try_single_file_connection(path);
        let timed_out = tokio::time::Instant::now() >= deadline;
        match outcome {
            Ok((connection, mode)) if mode.eq_ignore_ascii_case("delete") => return Ok(connection),
            Ok((_, mode)) if timed_out => {
                bail!(
                    "{} is still in journal mode {mode}; another connection may still be open",
                    path.display()
                );
            }
            Err(source) if timed_out => {
                return Err(source).with_context(|| format!("opening database {}", path.display()));
            }
            Ok(_) | Err(_) => tokio::task::yield_now().await,
        }
    }
}

fn try_single_file_connection(path: &Path) -> rusqlite::Result<(Connection, String)> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(LOCK_WAIT_MAX)?;
    let mode = connection.query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))?;
    Ok((connection, mode))
}

fn schema_version(connection: &Connection) -> anyhow::Result<DatabaseSchemaVersion> {
    let count: i64 = connection
        .query_row(
            &format!("SELECT COUNT(*) FROM {MIGRATION_LEDGER_TABLE}"),
            [],
            |row| row.get(0),
        )
        .context("reading the database's migration ledger")?;
    ensure!(
        count > 0,
        "no migrations are recorded in the migration ledger"
    );
    Ok(DatabaseSchemaVersion(count))
}

fn copy_file_new(source_path: &Path, destination_path: &Path) -> io::Result<()> {
    let mut source = File::open(source_path)?;
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination_path)?;
    io::copy(&mut source, &mut destination)?;
    destination.sync_all()
}

fn companion_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut path = database_path.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
}
