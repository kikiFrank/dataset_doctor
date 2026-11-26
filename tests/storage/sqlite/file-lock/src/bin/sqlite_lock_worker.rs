use rusqlite::{Connection, Error, ErrorCode, OpenFlags, ffi};
use std::env;
use std::error::Error as StdError;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

fn connect(database: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
    let journal: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    let foreign_keys: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    let synchronous: i64 = connection.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
    let busy: i64 = connection.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
    assert_eq!(journal, "wal");
    assert_eq!(foreign_keys, 1);
    assert_eq!(synchronous, 2);
    assert_eq!(busy, 250);
    eprintln!(
        "worker pid={} journal=wal foreign_keys=1 synchronous=FULL busy_timeout_ms={busy}",
        std::process::id()
    );
    Ok(connection)
}

fn rows(connection: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = connection.prepare("SELECT name FROM smoke_marker ORDER BY name")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    rows.collect()
}

fn signal(message: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "{message}")?;
    output.flush()
}

fn await_command(expected: &str) -> io::Result<()> {
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    if input.trim_end_matches(['\r', '\n']) != expected {
        return Err(io::Error::other(format!(
            "expected pipe command {expected}"
        )));
    }
    Ok(())
}

fn hold(database: &Path) -> Result<(), Box<dyn StdError>> {
    let connection = connect(database)?;
    connection.execute_batch("BEGIN IMMEDIATE; INSERT INTO smoke_marker VALUES ('holder');")?;
    assert!(!connection.is_autocommit());
    signal("LOCKED")?;
    await_command("commit")?;
    connection.execute_batch("COMMIT;")?;
    signal("COMMITTED")?;
    Ok(())
}

fn contend(database: &Path) -> Result<(), Box<dyn StdError>> {
    let connection = connect(database)?;
    assert_eq!(rows(&connection)?, ["baseline"]);
    eprintln!("contender sees the baseline and cannot see the holder's uncommitted row");

    let start = Instant::now();
    let attempt = connection
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO smoke_marker VALUES ('blocked'); COMMIT;");
    let elapsed = start.elapsed();
    let Err(Error::SqliteFailure(code, _)) = attempt else {
        return Err(
            format!("expected SQLITE_BUSY while the holder holds the lock: {attempt:?}").into(),
        );
    };
    assert_eq!(code.code, ErrorCode::DatabaseBusy);
    assert_eq!(code.extended_code & 0xff, ffi::SQLITE_BUSY);
    assert!(
        elapsed < Duration::from_secs(3),
        "SQLITE_BUSY was not bounded"
    );
    assert!(connection.is_autocommit());
    assert_eq!(rows(&connection)?, ["baseline"]);
    eprintln!(
        "contender SQLITE_BUSY primary={} elapsed_ms={} no committed side effect",
        code.extended_code & 0xff,
        elapsed.as_millis()
    );
    drop(connection);
    signal("BUSY")?;
    await_command("retry")?;

    let connection = connect(database)?;
    assert_eq!(rows(&connection)?, ["baseline", "holder"]);
    connection
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO smoke_marker VALUES ('contender'); COMMIT;")?;
    signal("WRITTEN")?;
    Ok(())
}

fn main() -> Result<(), Box<dyn StdError>> {
    let mut arguments = env::args_os().skip(1);
    let role = arguments.next().ok_or("missing worker role")?;
    let database = arguments.next().ok_or("missing synthetic database path")?;
    if arguments.next().is_some() {
        return Err("unexpected worker arguments".into());
    }
    match role.to_str() {
        Some("holder") => hold(Path::new(&database)),
        Some("contender") => contend(Path::new(&database)),
        _ => Err("unknown worker role".into()),
    }
}
