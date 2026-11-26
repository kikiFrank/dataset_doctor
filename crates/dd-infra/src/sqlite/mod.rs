//! SQLite connection setup and schema migrations for Core.

mod app_migration;
mod connection;
mod migration;

use rusqlite::{Row, types::Type};
use uuid::Uuid;

pub use app_migration::{AppMigrationError, migrate_app_database};
pub use connection::{
    WorkspaceConnectionError, WorkspaceConnectionStage, initialize_workspace_write_connection,
};
pub(crate) use migration::open_existing_workspace_database;
pub use migration::{WorkspaceMigrationError, WorkspaceMigrationStage, migrate_workspace_database};

pub(crate) fn row_uuid(row: &Row<'_>, index: usize) -> rusqlite::Result<Uuid> {
    let value: String = row.get(index)?;
    decode_uuid(&value, index)
}

pub(crate) fn decode_uuid(value: &str, index: usize) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
    })
}
