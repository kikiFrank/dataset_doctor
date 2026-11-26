use std::env;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;

use rusqlite::{Connection, ffi};

pub fn database_path(case: &str, filename: &str) -> PathBuf {
    let run = PathBuf::from(
        env::var_os("DD_SQLITE_MIGRATION_RUN_DIR").expect("set the SQLite migration run directory"),
    )
    .canonicalize()
    .expect("existing migration run directory");
    let runs = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/p01-04/runs")
        .canonicalize()
        .expect("canonical SQLite migration runs directory");
    assert_eq!(run.parent(), Some(runs.as_path()));
    let directory = run.join(case);
    fs::create_dir(&directory).expect("fresh synthetic case directory");
    directory.join(filename)
}

unsafe extern "C" fn deny_foreign_key_check(
    _: *mut c_void,
    action: c_int,
    name: *const c_char,
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
) -> c_int {
    // SAFETY: For SQLITE_PRAGMA, SQLite provides a callback-scoped C string.
    if action == ffi::SQLITE_PRAGMA
        && !name.is_null()
        && unsafe { CStr::from_ptr(name) }.to_bytes() == b"foreign_key_check"
    {
        ffi::SQLITE_DENY
    } else {
        ffi::SQLITE_OK
    }
}

pub fn fail_foreign_key_check(connection: &Connection) {
    // SAFETY: The live connection owns the SQLite handle. The callback holds no state.
    let status = unsafe {
        ffi::sqlite3_set_authorizer(
            connection.handle(),
            Some(deny_foreign_key_check),
            ptr::null_mut(),
        )
    };
    assert_eq!(status, ffi::SQLITE_OK);
}
