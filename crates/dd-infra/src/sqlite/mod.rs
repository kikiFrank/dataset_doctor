//! SQLite connection setup and schema migrations for Core.

mod app_migration;
mod connection;
mod migration;

pub use app_migration::{AppMigrationError, migrate_app_database};
pub use connection::{
    WorkspaceConnectionError, WorkspaceConnectionStage, initialize_workspace_write_connection,
};
pub use migration::{WorkspaceMigrationError, WorkspaceMigrationStage, migrate_workspace_database};
