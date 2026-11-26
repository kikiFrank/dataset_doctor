//! SQLite connection setup for the Workspace writer.

mod connection;

pub use connection::{
    WorkspaceConnectionError, WorkspaceConnectionStage, initialize_workspace_write_connection,
};
