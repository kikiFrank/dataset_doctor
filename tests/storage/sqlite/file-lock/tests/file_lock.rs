use rusqlite::{Connection, OpenFlags};
use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
const SOURCE_ID: &str =
    "2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76";

struct Worker {
    role: &'static str,
    child: Child,
    input: ChildStdin,
    output: Receiver<io::Result<String>>,
}

impl Worker {
    fn start(role: &'static str, database: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sqlite_lock_worker"))
            .arg(role)
            .arg(database)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|error| panic!("start {role}: {error}"));
        let input = child.stdin.take().expect("worker stdin");
        let output = child.stdout.take().expect("worker stdout");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        println!("{role} pid={}", child.id());
        Self {
            role,
            child,
            input,
            output: receiver,
        }
    }

    fn expect(&self, expected: &str) {
        let actual = self
            .output
            .recv_timeout(PROCESS_TIMEOUT)
            .unwrap_or_else(|error| panic!("{} waiting for {expected}: {error}", self.role))
            .expect("read worker signal");
        assert_eq!(actual, expected, "{} pipe signal", self.role);
        println!("{} acknowledged {expected}", self.role);
    }

    fn send(&mut self, command: &str) {
        writeln!(self.input, "{command}").expect("write worker command");
        self.input.flush().expect("flush worker command");
    }

    fn finish(&mut self) {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll worker exit") {
                assert!(status.success(), "{} exited with {status}", self.role);
                println!("{} exited with {status}", self.role);
                return;
            }
            assert!(Instant::now() < deadline, "{} did not exit", self.role);
            // Exit polling only. All database coordination uses pipe messages.
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn configure(connection: &Connection) -> rusqlite::Result<()> {
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
    println!("test connection journal=wal foreign_keys=1 synchronous=FULL busy_timeout_ms={busy}");
    Ok(())
}

fn rows(connection: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = connection.prepare("SELECT name FROM smoke_marker ORDER BY name")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    rows.collect()
}

#[test]
fn windows_wal_file_lock_between_processes() -> Result<(), Box<dyn std::error::Error>> {
    let run = PathBuf::from(env::var_os("DD_SQLITE_SMOKE_DIR").expect("use the P00-03 entry"))
        .canonicalize()?;
    let runs = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../target/p00-03/runs")
        .canonicalize()?;
    assert_eq!(
        run.parent(),
        Some(runs.as_path()),
        "unexpected run directory"
    );
    let database = run.join("synthetic-lock.sqlite3");
    assert!(!database.exists(), "expected a new synthetic database");

    let connection = Connection::open(&database)?;
    let journal: String = connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    assert_eq!(journal, "wal");
    configure(&connection)?;
    let (version, source_id): (String, String) =
        connection.query_row("SELECT sqlite_version(), sqlite_source_id()", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    println!("sqlite_version={version}");
    println!("sqlite_source_id={source_id}");
    assert_eq!(version, "3.54.1");
    assert_eq!(source_id, SOURCE_ID);
    connection.execute_batch(
        "CREATE TABLE smoke_marker (name TEXT PRIMARY KEY NOT NULL);
         INSERT INTO smoke_marker VALUES ('baseline');",
    )?;
    connection.close().map_err(|(_, error)| error)?;

    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    configure(&connection)?;
    assert_eq!(rows(&connection)?, ["baseline"]);
    connection.close().map_err(|(_, error)| error)?;

    let mut holder = Worker::start("holder", &database);
    holder.expect("LOCKED");
    let mut contender = Worker::start("contender", &database);
    assert_ne!(holder.child.id(), contender.child.id());
    assert_ne!(holder.child.id(), std::process::id());
    assert_ne!(contender.child.id(), std::process::id());
    contender.expect("BUSY");

    holder.send("commit");
    holder.expect("COMMITTED");
    holder.finish();
    contender.send("retry");
    contender.expect("WRITTEN");
    contender.finish();

    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    configure(&connection)?;
    assert_eq!(rows(&connection)?, ["baseline", "contender", "holder"]);
    connection.close().map_err(|(_, error)| error)?;
    println!(
        "reopened synthetic database contains baseline, contender, holder: {}",
        database.display()
    );
    Ok(())
}
