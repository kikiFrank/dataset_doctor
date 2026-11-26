mod support;

use dd_infra::sqlite::{AppMigrationError, migrate_app_database, migrate_workspace_database};
use rusqlite::{Connection, Error, ffi};
use support::{database_path, fail_foreign_key_check};

const WORKSPACE: &str = "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9";
const NOW: i64 = 1_794_000_000_000;

fn version(connection: &Connection) -> i64 {
    connection
        .pragma_query_value(Some("main"), "user_version", |row| row.get(0))
        .unwrap()
}

fn tables(connection: &Connection) -> Vec<String> {
    connection
        .prepare("SELECT name FROM main.sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn empty_global_database_initializes_and_reopens_without_overwriting_settings() {
    let path = database_path("app-initial", "app.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    let connection = migrate_app_database(connection, 250, NOW).unwrap();
    assert_eq!(version(&connection), 1);
    assert_eq!(tables(&connection), ["app_settings", "workspace_registry"]);
    let foreign_keys: i64 = connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 1);
    let original: (String, i64, i64) = connection
        .query_row(
            "SELECT config_json, config_revision, updated_at_ms FROM app_settings WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(original, ("{}".into(), 0, NOW));
    connection
        .execute("UPDATE app_settings SET config_revision=4", [])
        .unwrap();
    connection.close().unwrap();
    let reopened = migrate_app_database(Connection::open(&path).unwrap(), 400, NOW + 1).unwrap();
    let settings: (i64, i64) = reopened
        .query_row(
            "SELECT config_revision, updated_at_ms FROM app_settings",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(settings, (4, NOW));
    let integrity: String = reopened
        .query_row("PRAGMA main.integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    assert!(
        !reopened
            .prepare("PRAGMA main.foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
}

#[test]
fn rejects_workspace_databases_and_future_app_versions() {
    let workspace_path = database_path("app-reject-workspace", "app.db");
    let workspace = migrate_workspace_database(
        Connection::open(&workspace_path).unwrap(),
        250,
        WORKSPACE,
        NOW,
    )
    .unwrap();
    workspace.close().unwrap();
    assert!(matches!(
        migrate_app_database(Connection::open(&workspace_path).unwrap(), 250, NOW).unwrap_err(),
        AppMigrationError::UnrecognizedSchema
    ));
    let workspace = Connection::open(&workspace_path).unwrap();
    assert_eq!(version(&workspace), 0);
    assert!(tables(&workspace).contains(&"schema_meta".into()));

    let future_path = database_path("app-reject-version", "app.db");
    let future = migrate_app_database(Connection::open(&future_path).unwrap(), 250, NOW).unwrap();
    future
        .pragma_update(Some("main"), "user_version", 2)
        .unwrap();
    future.close().unwrap();
    assert!(matches!(
        migrate_app_database(Connection::open(&future_path).unwrap(), 250, NOW).unwrap_err(),
        AppMigrationError::UnsupportedVersion(2)
    ));
    assert_eq!(version(&Connection::open(&future_path).unwrap()), 2);
}

#[test]
fn rollbacks_global_schema_rows_and_version_after_late_failure() {
    let path = database_path("app-rollback", "app.db");
    let connection = Connection::open(&path).unwrap();
    fail_foreign_key_check(&connection);
    let error = migrate_app_database(connection, 250, NOW).unwrap_err();
    assert!(matches!(
        error,
        AppMigrationError::Sqlite {
            step: "foreign_key_check",
            source: Error::SqliteFailure(code, _),
        } if code.extended_code == ffi::SQLITE_AUTH
    ));
    let reopened = Connection::open(&path).unwrap();
    assert_eq!(version(&reopened), 0);
    assert!(tables(&reopened).is_empty());
    let retry = migrate_app_database(reopened, 250, NOW).unwrap();
    assert_eq!(version(&retry), 1);
}
