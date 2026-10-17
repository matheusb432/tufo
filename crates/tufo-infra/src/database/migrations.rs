//! Dedicated migration application and read-only runtime readiness checks.

use anyhow::{Context as _, bail};
use thiserror::Error;
use toasty::{
    Db,
    db::Executor,
    migration::{MigrationFile, MigrationSet},
    stmt::Value,
};
use toasty_core::driver::operation::TransactionMode;

static MIGRATIONS: MigrationSet = toasty::embed_migrations!();

const DATABASE_READY_REMEDY: &str =
    "restart tufo-server to migrate it; restore applied migrations if it reports catalogue drift";
const MIGRATION_REPAIR_REMEDY: &str =
    "restore the applied migration catalogue, or delete the database file if it is disposable";
const TOASTY_MIGRATIONS_TABLE: &str = "__toasty_migrations";

const CREATE_TOASTY_MIGRATIONS: &str = r"CREATE TABLE IF NOT EXISTS __toasty_migrations (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at TEXT NOT NULL
)";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MigrationIdentity {
    id: u64,
    name: String,
}

#[derive(Debug)]
enum MigrationLedgerState {
    Missing {
        schema_objects: Vec<String>,
    },
    Toasty {
        applied: Vec<MigrationIdentity>,
        schema_objects: Vec<String>,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
enum MigrationLedgerError {
    #[error("database schema objects exist without a Toasty migration ledger: {schema_objects:?}")]
    SchemaWithoutLedger { schema_objects: Vec<String> },
    #[error("database schema objects exist without applied Toasty migrations: {schema_objects:?}")]
    SchemaWithoutAppliedMigrations { schema_objects: Vec<String> },
    #[error(
        "the Toasty migration ledger is not an exact catalogue prefix; expected {expected:?}, found {applied:?}"
    )]
    ToastyNotPrefix {
        expected: Vec<MigrationIdentity>,
        applied: Vec<MigrationIdentity>,
    },
    #[error("the Toasty migration ledger is missing")]
    MissingToastyLedger,
    #[error("migration {sequence} ({name}) is pending")]
    Pending { sequence: usize, name: String },
}

pub(super) async fn migrate_database(database: &Db) -> anyhow::Result<()> {
    let expected = migration_catalogue();
    initialize_toasty_ledger(database, &expected)
        .await
        .map_err(migration_not_safe)?;

    while let MigrationApplyOutcome::Applied = apply_next_migration(database, &expected)
        .await
        .context("running database migrations")?
    {}

    check_database_ready(database)
        .await
        .context("verifying applied database migrations")
}

pub(super) async fn check_database_ready(db: &Db) -> anyhow::Result<()> {
    let mut db = db.clone();
    let expected = migration_catalogue();
    let state = read_migration_ledger(&mut db)
        .await
        .context("reading the Toasty migration ledger")
        .map_err(database_not_ready)?;

    if matches!(
        &state,
        MigrationLedgerState::Missing { schema_objects } if schema_objects.is_empty()
    ) {
        return Err(database_not_ready(
            MigrationLedgerError::MissingToastyLedger,
        ));
    }

    let applied_count = validate_migration_prefix(&expected, &state).map_err(database_not_ready)?;
    if let Some(pending) = expected.get(applied_count) {
        return Err(database_not_ready(MigrationLedgerError::Pending {
            sequence: applied_count + 1,
            name: pending.name.clone(),
        }));
    }

    Ok(())
}

async fn initialize_toasty_ledger(
    database: &Db,
    expected: &[MigrationIdentity],
) -> anyhow::Result<()> {
    let mut database = database.clone();
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await
        .context("locking the Toasty migration ledger")?;
    let state = read_migration_ledger(&mut transaction).await?;
    validate_migration_prefix(expected, &state)?;

    toasty::sql::statement(CREATE_TOASTY_MIGRATIONS)
        .exec(&mut transaction)
        .await
        .context("creating the Toasty migration ledger")?;

    transaction
        .commit()
        .await
        .context("committing Toasty migration ledger initialization")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationApplyOutcome {
    Applied,
    Complete,
}

async fn apply_next_migration(
    database: &Db,
    expected: &[MigrationIdentity],
) -> anyhow::Result<MigrationApplyOutcome> {
    let mut database = database.clone();
    let mut transaction = database
        .transaction_builder()
        .mode(TransactionMode::Immediate)
        .begin()
        .await
        .context("locking the Toasty migration ledger")?;
    let state = read_migration_ledger(&mut transaction).await?;
    if matches!(state, MigrationLedgerState::Missing { .. }) {
        return Err(MigrationLedgerError::MissingToastyLedger.into());
    }
    let applied_count = validate_migration_prefix(expected, &state)?;

    let Some(record) = expected.get(applied_count) else {
        transaction
            .commit()
            .await
            .context("releasing the database migration lock")?;
        return Ok(MigrationApplyOutcome::Complete);
    };
    let migration = &MIGRATIONS.migrations()[applied_count];
    apply_migration_sql(&mut transaction, migration)
        .await
        .with_context(|| format!("applying migration {} ({})", applied_count + 1, record.name))?;

    let foreign_key_violations = toasty::sql::query("PRAGMA foreign_key_check")
        .exec(&mut transaction)
        .await
        .context("checking foreign keys after migration")?;
    if !foreign_key_violations.is_empty() {
        bail!("migration introduced foreign-key violations: {foreign_key_violations:?}");
    }

    insert_toasty_migration(&mut transaction, record).await?;
    transaction
        .commit()
        .await
        .with_context(|| format!("committing migration {}", applied_count + 1))?;

    Ok(MigrationApplyOutcome::Applied)
}

async fn apply_migration_sql(
    executor: &mut dyn Executor,
    migration: &MigrationFile,
) -> anyhow::Result<()> {
    let migration = toasty_core::schema::db::Migration::new_sql(migration.sql().to_owned());
    for statement in migration.statements() {
        toasty::sql::statement(statement)
            .exec(executor)
            .await
            .context("executing migration statement")?;
    }
    Ok(())
}

async fn insert_toasty_migration(
    executor: &mut dyn Executor,
    migration: &MigrationIdentity,
) -> anyhow::Result<()> {
    toasty::sql::statement(
        "INSERT INTO __toasty_migrations (id, name, applied_at) VALUES (?1, ?2, datetime('now'))",
    )
    .bind(migration_id_i64(migration.id)?)
    .bind(migration.name.clone())
    .exec(executor)
    .await
    .context("recording the Toasty migration")?;
    Ok(())
}

async fn read_migration_ledger(
    executor: &mut dyn Executor,
) -> anyhow::Result<MigrationLedgerState> {
    let schema_objects = application_schema_objects(executor).await?;
    if !table_exists(executor, TOASTY_MIGRATIONS_TABLE).await? {
        return Ok(MigrationLedgerState::Missing { schema_objects });
    }

    Ok(MigrationLedgerState::Toasty {
        applied: read_toasty_migrations(executor).await?,
        schema_objects,
    })
}

async fn table_exists(executor: &mut dyn Executor, table: &str) -> anyhow::Result<bool> {
    let rows = toasty::sql::query(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
    )
    .bind(table.to_owned())
    .column_types([toasty::stmt::Type::I64])
    .exec(executor)
    .await
    .with_context(|| format!("checking for database table {table}"))?;
    decode_single_i64(&rows, "table existence").map(|value| value != 0)
}

async fn application_schema_objects(executor: &mut dyn Executor) -> anyhow::Result<Vec<String>> {
    let rows = toasty::sql::query(
        "SELECT type, name FROM sqlite_schema WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%' AND name != '__toasty_migrations' ORDER BY type, name",
    )
    .column_types([toasty::stmt::Type::String, toasty::stmt::Type::String])
    .exec(executor)
    .await
    .context("reading application schema objects")?;

    rows.into_iter()
        .map(|row| {
            let Value::Record(record) = row else {
                bail!("SQLite returned an invalid schema object row");
            };
            let [Value::String(object_type), Value::String(name)] = record.as_slice() else {
                bail!("SQLite returned an invalid schema object row");
            };
            Ok(format!("{object_type}:{name}"))
        })
        .collect()
}

async fn read_toasty_migrations(
    executor: &mut dyn Executor,
) -> anyhow::Result<Vec<MigrationIdentity>> {
    let rows = toasty::sql::query("SELECT id, name FROM __toasty_migrations ORDER BY id")
        .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::String])
        .exec(executor)
        .await
        .context("reading applied Toasty migrations")?;

    rows.into_iter()
        .map(|row| {
            let Value::Record(record) = row else {
                bail!("the Toasty migration ledger contains an invalid row");
            };
            let [Value::I64(id), Value::String(name)] = record.as_slice() else {
                bail!("the Toasty migration ledger contains an invalid row");
            };
            Ok(MigrationIdentity {
                id: u64::try_from(*id)
                    .context("the Toasty migration ledger contains a negative migration ID")?,
                name: name.clone(),
            })
        })
        .collect()
}

fn migration_catalogue() -> Vec<MigrationIdentity> {
    MIGRATIONS
        .migrations()
        .iter()
        .map(|migration| MigrationIdentity {
            id: migration.id(),
            name: migration.name().to_owned(),
        })
        .collect()
}

fn validate_migration_prefix(
    expected: &[MigrationIdentity],
    state: &MigrationLedgerState,
) -> Result<usize, MigrationLedgerError> {
    match state {
        MigrationLedgerState::Missing { schema_objects } => {
            if schema_objects.is_empty() {
                Ok(0)
            } else {
                Err(MigrationLedgerError::SchemaWithoutLedger {
                    schema_objects: schema_objects.clone(),
                })
            }
        }
        MigrationLedgerState::Toasty {
            applied,
            schema_objects,
        } => {
            if applied.is_empty() && !schema_objects.is_empty() {
                return Err(MigrationLedgerError::SchemaWithoutAppliedMigrations {
                    schema_objects: schema_objects.clone(),
                });
            }
            validate_toasty_prefix(expected, applied)
        }
    }
}

fn validate_toasty_prefix(
    expected: &[MigrationIdentity],
    applied: &[MigrationIdentity],
) -> Result<usize, MigrationLedgerError> {
    let expected_prefix = expected.get(..applied.len()).unwrap_or(expected).to_vec();
    let mut expected_sorted = expected_prefix.clone();
    expected_sorted.sort();
    let mut applied_sorted = applied.to_vec();
    applied_sorted.sort();

    if expected_sorted != applied_sorted {
        return Err(MigrationLedgerError::ToastyNotPrefix {
            expected: expected_prefix,
            applied: applied.to_vec(),
        });
    }

    Ok(applied.len())
}

fn decode_single_i64(rows: &[Value], description: &str) -> anyhow::Result<i64> {
    match rows {
        [Value::Record(record)] => match record.as_slice() {
            [Value::I64(value)] => Ok(*value),
            _ => bail!("{description} returned an invalid row: {rows:?}"),
        },
        _ => bail!("{description} returned an invalid row: {rows:?}"),
    }
}

fn migration_id_i64(id: u64) -> anyhow::Result<i64> {
    i64::try_from(id).context("migration ID exceeds SQLite's signed integer range")
}

fn database_not_ready(error: impl Into<anyhow::Error>) -> anyhow::Error {
    error.into().context(format!(
        "database schema is not ready; {DATABASE_READY_REMEDY}"
    ))
}

fn migration_not_safe(error: impl Into<anyhow::Error>) -> anyhow::Error {
    error.into().context(format!(
        "refusing to apply migrations; {MIGRATION_REPAIR_REMEDY}"
    ))
}

#[cfg(test)]
mod tests {
    use super::MIGRATIONS;

    #[test]
    fn embedded_migrations_have_lf_line_endings() {
        for migration in MIGRATIONS.migrations() {
            assert!(
                !migration.sql().contains('\r'),
                "{} has CRLF line endings, which hide its statement breakpoints",
                migration.name()
            );
        }
    }
}
