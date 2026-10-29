use std::sync::atomic::AtomicBool;

use qwerio_core::connection::{ConnectionOptions, Credentials, SqliteOptions};
use rusqlite::Connection;

use super::*;

fn sqlite_request(source: &std::path::Path, destination: &std::path::Path) -> TransferRequest {
    TransferRequest {
        options: ConnectionOptions::Sqlite(SqliteOptions::new(source)),
        credentials: Credentials::none(),
        database: None,
        operation: Operation::Backup,
        format: Format::Archive,
        file: destination.to_owned(),
        restored_file: None,
        tool: None,
    }
}

#[test]
fn backs_up_live_wal_data_and_restores_into_a_new_file() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.sqlite");
    let backup = directory.path().join("backup.sqlite");
    let restored = directory.path().join("restored.sqlite");
    let connection = Connection::open(&source).expect("database");
    connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE items (name TEXT); INSERT INTO items VALUES ('hello');").expect("seed");
    run(sqlite_request(&source, &backup), &AtomicBool::new(false)).expect("backup");
    connection
        .execute("DELETE FROM items", [])
        .expect("change source");
    let request = TransferRequest {
        operation: Operation::Restore,
        file: backup,
        restored_file: Some(restored.clone()),
        ..sqlite_request(&source, &restored)
    };
    assert_eq!(
        run(request, &AtomicBool::new(false)).expect("restore"),
        restored
    );
    let copy = Connection::open(restored).expect("restored database");
    assert_eq!(
        copy.query_row("SELECT name FROM items", [], |row| row.get::<_, String>(0))
            .expect("row"),
        "hello"
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0))
            .expect("count"),
        0
    );
}

#[test]
fn refuses_existing_destinations_and_cleans_up_failed_backups() {
    let directory = tempfile::tempdir().expect("directory");
    let invalid = directory.path().join("invalid.sqlite");
    let destination = directory.path().join("backup.sqlite");
    std::fs::write(&invalid, "not a database").expect("invalid source");
    assert!(
        run(
            sqlite_request(&invalid, &destination),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert!(!destination.exists());
    assert_eq!(
        std::fs::read_dir(directory.path()).expect("files").count(),
        1
    );
    std::fs::write(&destination, "keep this").expect("existing destination");
    assert!(
        run(
            sqlite_request(&invalid, &destination),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(destination).expect("original file"),
        "keep this"
    );
}

#[test]
fn cancellation_does_not_create_a_destination() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.sqlite");
    let destination = directory.path().join("backup.sqlite");
    assert!(
        run(
            sqlite_request(&source, &destination),
            &AtomicBool::new(true)
        )
        .is_err()
    );
    assert!(!destination.exists());
}

#[test]
fn selects_engine_specific_clients_and_formats() {
    use tools::NativeTool;
    assert_eq!(
        Format::Archive.tool(EngineKind::Postgres, Operation::Restore),
        Some(NativeTool::PgRestore)
    );
    assert_eq!(
        Format::Sql.tool(EngineKind::Postgres, Operation::Restore),
        Some(NativeTool::Psql)
    );
    assert_eq!(
        Format::Sql.tool(EngineKind::Mariadb, Operation::Backup),
        Some(NativeTool::MariadbDump)
    );
    assert_eq!(
        Format::Archive.tool(EngineKind::Sqlite, Operation::Backup),
        None
    );
}
