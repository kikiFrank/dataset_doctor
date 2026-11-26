//! SQLite connection setup and schema migration for the Workspace writer.

mod connection;
mod migration;

pub use connection::{
    WorkspaceConnectionError, WorkspaceConnectionStage, initialize_workspace_write_connection,
};
pub use migration::{WorkspaceMigrationError, WorkspaceMigrationStage, migrate_workspace_database};
