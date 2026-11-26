use std::cell::Cell;
use std::env;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;

use dd_infra::sqlite::{
    WorkspaceConnectionError, WorkspaceConnectionStage, initialize_workspace_write_connection,
};
use rusqlite::{Connection, Error, OpenFlags, ffi};

const SQLITE_SOURCE_ID: &str =
    "2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76";

fn database_path(case: &str) -> PathBuf {
    let run =
        PathBuf::from(env::var_os("DD_SQLITE_INIT_RUN_DIR").expect("set the P01-03 run directory"))
            .canonicalize()
            .expect("existing run directory");
    let runs = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../target/p01-03/runs")
        .canonicalize()
        .expect("canonical P01-03 runs directory");
    assert_eq!(
        run.parent(),
        Some(runs.as_path()),
        "unexpected run directory"
    );
    let directory = run.join(case);
    fs::create_dir(&directory).expect("create a fresh synthetic case directory");
    directory.join("workspace.db")
}

fn journal_mode(connection: &Connection) -> String {
    connection
        .pragma_query_value(Some("main"), "journal_mode", |row| row.get(0))
        .unwrap()
}

fn assert_settings(connection: &Connection, stage: &str, expected: (i64, &str, i64, i64)) {
    let foreign_keys: i64 = connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap();
    let journal = journal_mode(connection);
    let synchronous: i64 = connection
        .pragma_query_value(Some("main"), "synchronous", |row| row.get(0))
        .unwrap();
    let busy_timeout: i64 = connection
        .pragma_query_value(None, "busy_timeout", |row| row.get(0))
        .unwrap();
    println!(
        "{stage}: foreign_keys={foreign_keys} journal={journal} synchronous={synchronous} busy_timeout_ms={busy_timeout}"
    );
    assert_eq!(
        (foreign_keys, journal.as_str(), synchronous, busy_timeout),
        expected
    );
}

#[test]
fn configures_a_file_and_reapplies_connection_settings_on_reopen() {
    let path = database_path("initialize-and-reopen");
    let connection = Connection::open(&path).unwrap();
    let initial_journal: String = connection
        .query_row("PRAGMA main.journal_mode=DELETE", [], |row| row.get(0))
        .unwrap();
    assert_eq!(initial_journal, "delete");
    connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF; PRAGMA main.synchronous=NORMAL; PRAGMA busy_timeout=17;",
        )
        .unwrap();
    assert_settings(&connection, "initial connection", (0, "delete", 1, 17));

    let connection = initialize_workspace_write_connection(connection, 250).unwrap();
    assert_settings(&connection, "initialized connection", (1, "wal", 2, 250));
    let (version, source_id): (String, String) = connection
        .query_row("SELECT sqlite_version(), sqlite_source_id()", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    println!("sqlite_version={version}");
    println!("sqlite_source_id={source_id}");
    assert_eq!(version, "3.54.1");
    assert_eq!(source_id, SQLITE_SOURCE_ID);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0,
        "initialization created a schema or migration table"
    );

    connection
        .execute_batch(
            "CREATE TABLE synthetic_parent (id INTEGER PRIMARY KEY);
             CREATE TABLE synthetic_child (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL
                 REFERENCES synthetic_parent(id));
             INSERT INTO synthetic_parent VALUES (1);
             INSERT INTO synthetic_child VALUES (1, 1);",
        )
        .unwrap();
    let orphan = connection
        .execute("INSERT INTO synthetic_child VALUES (2, 999)", [])
        .expect_err("orphan insert should fail");
    let Error::SqliteFailure(code, _) = orphan else {
        panic!("expected a SQLite foreign-key failure, got {orphan:?}");
    };
    assert_eq!(code.extended_code, ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    let child_count: i64 = connection
        .query_row("SELECT count(*) FROM synthetic_child", [], |row| row.get(0))
        .unwrap();
    assert_eq!(child_count, 1);
    println!(
        "valid foreign key committed; orphan rejected with SQLite extended code {}",
        code.extended_code
    );
    connection.close().unwrap();

    let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    assert_eq!(
        journal_mode(&connection),
        "wal",
        "WAL did not persist on reopen"
    );
    let child: (i64, i64) = connection
        .query_row("SELECT id, parent_id FROM synthetic_child", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(child, (1, 1));
    connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF; PRAGMA main.synchronous=NORMAL; PRAGMA busy_timeout=31;",
        )
        .unwrap();
    assert_settings(
        &connection,
        "reopened pre-initialization",
        (0, "wal", 1, 31),
    );
    let connection = initialize_workspace_write_connection(connection, 400).unwrap();
    assert_settings(&connection, "reinitialized connection", (1, "wal", 2, 400));
    connection.close().unwrap();
}

unsafe extern "C" fn observe_pragma(
    context: *mut c_void,
    action: c_int,
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
) -> c_int {
    if action == ffi::SQLITE_PRAGMA {
        // SAFETY: reject_before_pragma gives SQLite a live, thread-local Cell
        // which outlives its use of the authorizer.
        unsafe { &*context.cast::<Cell<bool>>() }.set(true);
    }
    ffi::SQLITE_OK
}

fn reject_before_pragma(connection: Connection, timeout: u32) -> WorkspaceConnectionError {
    let seen = Cell::new(false);
    // SAFETY: SQLite owns the handle. The callback uses this Cell only on this
    // thread while the initializer owns and drops the connection.
    let status = unsafe {
        ffi::sqlite3_set_authorizer(
            connection.handle(),
            Some(observe_pragma),
            ptr::from_ref(&seen).cast_mut().cast(),
        )
    };
    assert_eq!(status, ffi::SQLITE_OK);
    let error = initialize_workspace_write_connection(connection, timeout)
        .expect_err("invalid input should consume and reject the connection");
    assert!(!seen.get(), "invalid input attempted a PRAGMA");
    assert_eq!(error.stage(), WorkspaceConnectionStage::Input);
    println!("rejected before PRAGMA: {error}");
    error
}

#[test]
fn rejects_in_memory_main() {
    assert!(matches!(
        reject_before_pragma(Connection::open_in_memory().unwrap(), 250),
        WorkspaceConnectionError::NotFileBacked
    ));
}

#[test]
fn rejects_read_only_file() {
    let path = database_path("read-only");
    let seed = Connection::open(&path).unwrap();
    seed.execute("CREATE TABLE synthetic_marker (id INTEGER)", [])
        .unwrap();
    seed.close().unwrap();
    let read_only = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert!(read_only.is_readonly("main").unwrap());
    assert!(matches!(
        reject_before_pragma(read_only, 250),
        WorkspaceConnectionError::ReadOnlyMain
    ));
}

#[test]
fn rejects_active_transaction_and_drops_its_uncommitted_rows() {
    let path = database_path("active-transaction");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE synthetic_marker (id INTEGER PRIMARY KEY);
             INSERT INTO synthetic_marker VALUES (1);
             BEGIN IMMEDIATE;
             INSERT INTO synthetic_marker VALUES (2);",
        )
        .unwrap();
    assert!(!connection.is_autocommit());
    assert!(matches!(
        reject_before_pragma(connection, 250),
        WorkspaceConnectionError::ActiveTransaction
    ));
    let reopened = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    let (count, last): (i64, i64) = reopened
        .query_row(
            "SELECT count(*), max(id) FROM synthetic_marker",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((count, last), (1, 1));
    println!("reopened transaction fixture retains the committed row only");
}

#[test]
fn rejects_zero_and_unrepresentable_busy_timeouts() {
    let path = database_path("invalid-timeout");
    Connection::open(&path).unwrap().close().unwrap();
    for timeout in [0, i32::MAX as u32 + 1] {
        let connection =
            Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
        assert!(matches!(
            reject_before_pragma(connection, timeout),
            WorkspaceConnectionError::InvalidBusyTimeout { requested_ms } if requested_ms == timeout
        ));
    }
}

unsafe extern "C" fn deny_synchronous(
    _: *mut c_void,
    action: c_int,
    name: *const c_char,
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
) -> c_int {
    // SAFETY: For SQLITE_PRAGMA SQLite supplies the name as a NUL-terminated
    // string valid for this callback.
    if action == ffi::SQLITE_PRAGMA
        && !name.is_null()
        && unsafe { CStr::from_ptr(name) }.to_bytes() == b"synchronous"
    {
        ffi::SQLITE_DENY
    } else {
        ffi::SQLITE_OK
    }
}

#[test]
fn reports_later_sqlite_failure_with_stage_and_preserves_persisted_wal() {
    let path = database_path("later-sqlite-failure");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("CREATE TABLE synthetic_marker (id INTEGER)", [])
        .unwrap();
    assert_eq!(journal_mode(&connection), "delete");
    // SAFETY: SQLite owns the handle. The test-only callback keeps no state.
    let status = unsafe {
        ffi::sqlite3_set_authorizer(connection.handle(), Some(deny_synchronous), ptr::null_mut())
    };
    assert_eq!(status, ffi::SQLITE_OK);
    let error = initialize_workspace_write_connection(connection, 250)
        .expect_err("SQLite should reject the synchronous PRAGMA");
    assert_eq!(error.stage(), WorkspaceConnectionStage::Synchronous);
    let WorkspaceConnectionError::Sqlite {
        source: Error::SqliteFailure(code, _),
        ..
    } = error
    else {
        panic!("expected the underlying SQLite error, got {error:?}");
    };
    assert_eq!(code.extended_code & 0xff, ffi::SQLITE_AUTH);
    let reopened = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    assert_eq!(journal_mode(&reopened), "wal");
    println!(
        "synchronous stage preserved SQLite error code {}; WAL persisted",
        code.extended_code
    );
}
