use std::path::Path;

mod support;

use dd_infra::sqlite::{
    WorkspaceMigrationError, WorkspaceMigrationStage, migrate_workspace_database,
};
use rusqlite::{Connection, Error, ffi};
use support::{database_path, fail_foreign_key_check};

const WORKSPACE: &str = "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9";
const OTHER_WORKSPACE: &str = "76afdb2d-0900-4528-b303-0fca0f864235";
const CREATED_AT: i64 = 1_794_000_000_000;
const BUSY_TIMEOUT_MS: u32 = 250;

fn migrate(path: &Path) -> Result<Connection, WorkspaceMigrationError> {
    migrate_workspace_database(
        Connection::open(path).unwrap(),
        BUSY_TIMEOUT_MS,
        WORKSPACE,
        CREATED_AT,
    )
}

fn read_names(connection: &Connection, object_type: &str) -> Vec<String> {
    connection
        .prepare(
            "SELECT name FROM main.sqlite_schema
             WHERE type = ?1 AND name NOT GLOB 'sqlite_*'
             ORDER BY name",
        )
        .unwrap()
        .query_map([object_type], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn metadata(connection: &Connection) -> (i64, String, i64, i64, i64) {
    connection
        .query_row(
            "SELECT singleton, workspace_id, schema_version, created_at_ms, updated_at_ms
             FROM main.schema_meta",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap()
}

fn journal_mode(connection: &Connection) -> String {
    connection
        .pragma_query_value(Some("main"), "journal_mode", |row| row.get(0))
        .unwrap()
}

#[test]
fn empty_file_has_v1_schema_and_passes_integrity_and_foreign_key_checks() {
    let path = database_path("empty", "workspace.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=17;",
        )
        .unwrap();
    let connection =
        migrate_workspace_database(connection, BUSY_TIMEOUT_MS, WORKSPACE, CREATED_AT).unwrap();
    assert_eq!(journal_mode(&connection), "wal");
    let foreign_keys: i64 = connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 1);
    connection.close().unwrap();

    let reopened = Connection::open(&path).unwrap();
    assert_eq!(
        metadata(&reopened),
        (1, WORKSPACE.to_owned(), 1, CREATED_AT, CREATED_AT)
    );
    assert_eq!(
        read_names(&reopened, "table"),
        [
            "assets",
            "dataset_assets",
            "datasets",
            "file_operations",
            "review_sessions",
            "schema_meta",
            "staging_items",
            "tasks",
            "workflow_runs",
        ]
    );
    assert_eq!(
        read_names(&reopened, "index"),
        [
            "idx_assets_availability",
            "idx_assets_source_video",
            "idx_file_operations_asset",
            "idx_file_operations_status",
            "idx_reviews_dataset_status",
            "idx_staging_target",
            "idx_tasks_status_created",
            "idx_tasks_workflow_node",
            "idx_workflows_dataset_status",
            "uq_assets_work_annotation",
            "uq_review_active_asset",
            "uq_tasks_current_attempt",
        ]
    );
    let reports = reopened
        .prepare("PRAGMA main.integrity_check")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(reports, ["ok"]);
    assert!(
        !reopened
            .prepare("PRAGMA main.foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    reopened.close().unwrap();

    let connection = migrate(&path).unwrap();
    let error = connection
        .execute(
            "INSERT INTO dataset_assets VALUES ('missing-dataset', 'missing-asset', NULL, 0)",
            [],
        )
        .unwrap_err();
    assert!(matches!(
        error,
        Error::SqliteFailure(code, _) if code.extended_code == ffi::SQLITE_CONSTRAINT_FOREIGNKEY
    ));
}

#[test]
fn reopening_a_matching_v1_preserves_metadata_and_business_rows() {
    let path = database_path("reopen", "workspace.db");
    let connection = migrate(&path).unwrap();
    connection
        .execute(
            "INSERT INTO datasets(dataset_id, name, created_at_ms, updated_at_ms)
             VALUES (?1, 'Synthetic', ?2, ?2)",
            rusqlite::params![OTHER_WORKSPACE, CREATED_AT],
        )
        .unwrap();
    connection.close().unwrap();
    let reopened = migrate_workspace_database(
        Connection::open(&path).unwrap(),
        400,
        WORKSPACE,
        CREATED_AT + 1,
    )
    .unwrap();
    assert_eq!(
        metadata(&reopened),
        (1, WORKSPACE.to_owned(), 1, CREATED_AT, CREATED_AT)
    );
    let name: String = reopened
        .query_row("SELECT name FROM datasets", [], |row| row.get(0))
        .unwrap();
    assert_eq!(name, "Synthetic");
}

#[test]
fn rejects_unrecognized_schema_without_changing_its_journal_or_tables() {
    let path = database_path("unrecognized", "workspace.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("CREATE TABLE unrelated(id INT)", [])
        .unwrap();
    connection.close().unwrap();
    assert!(matches!(
        migrate(&path).unwrap_err(),
        WorkspaceMigrationError::UnrecognizedSchema
    ));
    let reopened = Connection::open(&path).unwrap();
    assert_eq!(journal_mode(&reopened), "delete");
    assert_eq!(read_names(&reopened, "table"), ["unrelated"]);
}

#[test]
fn rejects_foreign_identity_and_future_version_without_changing_metadata() {
    let path = database_path("identity-and-version", "workspace.db");
    let connection = migrate(&path).unwrap();
    connection.close().unwrap();
    let error = migrate_workspace_database(
        Connection::open(&path).unwrap(),
        BUSY_TIMEOUT_MS,
        OTHER_WORKSPACE,
        CREATED_AT,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        WorkspaceMigrationError::WorkspaceIdMismatch
    ));

    let connection = Connection::open(&path).unwrap();
    connection
        .execute("UPDATE schema_meta SET schema_version=2", [])
        .unwrap();
    connection.close().unwrap();
    assert!(matches!(
        migrate(&path).unwrap_err(),
        WorkspaceMigrationError::UnsupportedVersion(2)
    ));
    assert_eq!(
        metadata(&Connection::open(&path).unwrap()),
        (1, WORKSPACE.to_owned(), 2, CREATED_AT, CREATED_AT)
    );
}

#[test]
fn late_migration_failure_rolls_back_schema_and_metadata_then_allows_retry() {
    let path = database_path("rollback", "workspace.db");
    let connection = Connection::open(&path).unwrap();
    fail_foreign_key_check(&connection);
    let error =
        migrate_workspace_database(connection, BUSY_TIMEOUT_MS, WORKSPACE, CREATED_AT).unwrap_err();
    assert!(matches!(
        error,
        WorkspaceMigrationError::Sqlite {
            stage: WorkspaceMigrationStage::ForeignKeyCheck,
            source: Error::SqliteFailure(code, _),
        } if code.extended_code == ffi::SQLITE_AUTH
    ));
    let reopened = Connection::open(&path).unwrap();
    let schema_objects: i64 = reopened
        .query_row("SELECT count(*) FROM main.sqlite_schema", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(schema_objects, 0, "migration left objects after rollback");
    reopened.close().unwrap();
    assert_eq!(metadata(&migrate(&path).unwrap()).2, 1);
}
