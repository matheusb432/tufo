use std::{
    borrow::Cow,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use toasty_core::{
    Result, Schema,
    driver::{
        Capability, ConnectContext, Connection, Driver, ExecResponse,
        operation::{Operation, RawSql, RawSqlRet},
    },
    schema::{app, db::Migration, diff},
};
use toasty_driver_sqlite::Sqlite;

#[derive(Debug, Clone, Copy)]
pub(super) enum DatabaseConnectionRole {
    Runtime,
    Migration,
}

#[derive(Debug)]
pub(super) struct ConfiguredSqlite {
    inner: Sqlite,
    path: PathBuf,
    role: DatabaseConnectionRole,
    configuration_schema: Arc<Schema>,
}

impl ConfiguredSqlite {
    pub(super) fn new(path: &Path, role: DatabaseConnectionRole) -> Result<Self> {
        let inner = Sqlite::open(path);
        let configuration_schema = toasty_core::schema::Builder::new()
            .build(app::Schema::default(), inner.capability())?;

        Ok(Self {
            inner,
            path: path.to_path_buf(),
            role,
            configuration_schema: Arc::new(configuration_schema),
        })
    }

    fn require_existing_database(&self) -> Result<()> {
        if matches!(self.role, DatabaseConnectionRole::Migration) {
            return Ok(());
        }

        let metadata =
            std::fs::metadata(&self.path).map_err(toasty_core::Error::driver_operation_failed)?;
        if metadata.is_file() {
            return Ok(());
        }

        Err(toasty_core::Error::driver_operation_failed(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("database path is not a file: {}", self.path.display()),
        )))
    }

    async fn configure_connection(&self, connection: &mut dyn Connection) -> Result<()> {
        self.execute_pragma(connection, "PRAGMA foreign_keys = ON", RawSqlRet::Infer)
            .await?;
        self.execute_pragma(connection, "PRAGMA busy_timeout = 5000", RawSqlRet::Infer)
            .await?;
        if matches!(self.role, DatabaseConnectionRole::Migration) {
            self.execute_pragma(connection, "PRAGMA journal_mode = WAL", RawSqlRet::Infer)
                .await?;
        }
        Ok(())
    }

    async fn execute_pragma(
        &self,
        connection: &mut dyn Connection,
        sql: &str,
        ret: RawSqlRet,
    ) -> Result<ExecResponse> {
        connection
            .exec(
                &self.configuration_schema,
                Operation::RawSql(RawSql {
                    sql: sql.to_owned(),
                    params: Vec::new(),
                    ret,
                }),
            )
            .await
    }
}

#[async_trait]
impl Driver for ConfiguredSqlite {
    fn url(&self) -> Cow<'_, str> {
        self.inner.url()
    }

    fn capability(&self) -> &'static Capability {
        self.inner.capability()
    }

    async fn connect(&self, context: &ConnectContext) -> Result<Box<dyn Connection>> {
        self.require_existing_database()?;
        let mut connection = self.inner.connect(context).await?;
        self.configure_connection(connection.as_mut()).await?;
        Ok(connection)
    }

    fn generate_migration(&self, schema_diff: &diff::Schema<'_>) -> Migration {
        self.inner.generate_migration(schema_diff)
    }

    async fn reset_db(&self) -> Result<()> {
        self.inner.reset_db().await
    }
}
