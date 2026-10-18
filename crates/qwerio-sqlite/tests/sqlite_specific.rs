//! SQLite behavior beyond the shared contract.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use qwerio_core::connection::{
    ConnectionDescriptor, ConnectionOptions, Credentials, SqliteOptions,
};
use qwerio_core::contract::{close, failure, final_transaction_state, rows, run};
use qwerio_core::driver::{DatabaseDriver, DatabaseSession, OpenSessionRequest};
use qwerio_core::error::ErrorKind;
use qwerio_core::ids::ConnectionId;
use qwerio_core::session::{AccessMode, ReadOnlyEnforcement, SessionPurpose, TransactionState};
use qwerio_core::value::Value;
use qwerio_sqlite::{SqliteDriver, create_database_file};
use tempfile::TempDir;

fn request(options: SqliteOptions, access: AccessMode) -> OpenSessionRequest {
    OpenSessionRequest {
        connection: ConnectionDescriptor {
            id: ConnectionId::new(),
            name: "test".to_owned(),
            options: ConnectionOptions::Sqlite(options),
        },
        credentials: Credentials::none(),
        purpose: SessionPurpose::Console,
        access,
        database: None,
    }
}

async fn open(path: &Path, access: AccessMode) -> Arc<dyn DatabaseSession> {
    SqliteDriver::new()
        .open(request(SqliteOptions::new(path), access))
        .await
        .expect("open session")
}

fn new_database(directory: &TempDir) -> std::path::PathBuf {
    let path = directory.path().join("test.sqlite");
    create_database_file(&path).expect("create database");
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn opening_a_missing_file_never_creates_it() {
    let directory = TempDir::new().expect("temporary directory");
    let path = directory.path().join("missing.sqlite");

    for access in [AccessMode::ReadOnly, AccessMode::ReadWrite] {
        let result = SqliteDriver::new()
            .open(request(SqliteOptions::new(&path), access))
            .await;

        assert_eq!(
            result.err().map(|error| error.kind),
            Some(ErrorKind::NotFound)
        );
        assert!(!path.exists(), "no file may be created by opening");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn creating_a_database_refuses_existing_files() {
    let directory = TempDir::new().expect("temporary directory");
    let path = new_database(&directory);

    let error = create_database_file(&path).expect_err("file exists");

    assert_eq!(error.kind, ErrorKind::InvalidRequest);
}

#[tokio::test(flavor = "multi_thread")]
async fn files_that_are_not_databases_fail_to_open() {
    let directory = TempDir::new().expect("temporary directory");
    let path = directory.path().join("notes.txt");
    std::fs::write(
        &path,
        "this is not a database, just enough text to fill a header page",
    )
    .expect("write");

    let error = SqliteDriver::new()
        .open(request(SqliteOptions::new(&path), AccessMode::ReadOnly))
        .await
        .err()
        .expect("not a database");

    assert_eq!(error.kind, ErrorKind::Connection);
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_keep_their_storage_class_regardless_of_declared_type() {
    let directory = TempDir::new().expect("temporary directory");
    let path = new_database(&directory);
    let session = open(&path, AccessMode::ReadWrite).await;

    run(session.as_ref(), "CREATE TABLE mixed (value INTEGER)").await;
    run(
        session.as_ref(),
        "INSERT INTO mixed VALUES (1), ('text'), (2.5), (X'01'), (NULL)",
    )
    .await;
    let events = run(session.as_ref(), "SELECT value FROM mixed ORDER BY rowid").await;

    let values: Vec<Value> = rows(&events).into_iter().flatten().collect();
    assert_eq!(
        values,
        vec![
            Value::Integer(1),
            Value::Text("text".to_owned()),
            Value::Float(2.5),
            Value::Bytes(vec![1]),
            Value::Null,
        ]
    );
    close(session.as_ref()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_sessions_use_read_only_open_flags() {
    let directory = TempDir::new().expect("temporary directory");
    let path = new_database(&directory);
    let session = open(&path, AccessMode::ReadOnly).await;

    assert_eq!(
        session.info().enforcement,
        ReadOnlyEnforcement::ReadOnlyFile
    );

    // Even pragmas that write the header fail at the engine level.
    let events = run(session.as_ref(), "PRAGMA user_version = 7").await;
    assert_eq!(
        failure(&events).map(|error| error.kind),
        Some(ErrorKind::ReadOnly)
    );
    close(session.as_ref()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn foreign_key_enforcement_follows_the_connection_option() {
    let directory = TempDir::new().expect("temporary directory");
    let path = new_database(&directory);
    let setup = open(&path, AccessMode::ReadWrite).await;
    run(
        setup.as_ref(),
        "CREATE TABLE parent (id INTEGER PRIMARY KEY)",
    )
    .await;
    run(
        setup.as_ref(),
        "CREATE TABLE child (parent_id INTEGER REFERENCES parent (id))",
    )
    .await;
    close(setup.as_ref()).await;

    let enforced = open(&path, AccessMode::ReadWrite).await;
    assert_eq!(enforced.info().foreign_keys_enforced, Some(true));
    let events = run(enforced.as_ref(), "INSERT INTO child VALUES (42)").await;
    assert_eq!(
        failure(&events).map(|error| error.kind),
        Some(ErrorKind::Constraint)
    );
    close(enforced.as_ref()).await;

    let options = SqliteOptions {
        enforce_foreign_keys: false,
        ..SqliteOptions::new(&path)
    };
    let relaxed = SqliteDriver::new()
        .open(request(options, AccessMode::ReadWrite))
        .await
        .expect("open");
    assert_eq!(relaxed.info().foreign_keys_enforced, Some(false));
    let events = run(relaxed.as_ref(), "INSERT INTO child VALUES (42)").await;
    assert_eq!(failure(&events), None);
    close(relaxed.as_ref()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn locked_databases_report_busy_after_the_timeout() {
    let directory = TempDir::new().expect("temporary directory");
    let path = new_database(&directory);
    let writer = open(&path, AccessMode::ReadWrite).await;
    run(writer.as_ref(), "CREATE TABLE t (id INTEGER)").await;

    let begin = run(writer.as_ref(), "BEGIN IMMEDIATE").await;
    assert_eq!(
        final_transaction_state(&begin),
        Some(TransactionState::Active)
    );

    let options = SqliteOptions {
        busy_timeout: Duration::from_millis(100),
        ..SqliteOptions::new(&path)
    };
    let other = SqliteDriver::new()
        .open(request(options, AccessMode::ReadWrite))
        .await
        .expect("open second connection");
    let events = run(other.as_ref(), "INSERT INTO t VALUES (1)").await;

    assert_eq!(
        failure(&events).map(|error| error.kind),
        Some(ErrorKind::Busy)
    );
    assert_eq!(
        final_transaction_state(&events),
        Some(TransactionState::Idle)
    );

    writer.rollback().await.expect("rollback");
    close(writer.as_ref()).await;
    close(other.as_ref()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rowid_tables_without_keys_page_by_rowid() {
    let directory = TempDir::new().expect("temporary directory");
    let path = new_database(&directory);
    let session = open(&path, AccessMode::ReadWrite).await;
    run(
        session.as_ref(),
        "CREATE TABLE plain (rowid_alias TEXT, \"oid\" TEXT)",
    )
    .await;

    let object = qwerio_core::resource::ObjectRef {
        connection: session.info().connection,
        database: None,
        namespace: None,
        kind: qwerio_core::resource::ObjectKind::Table,
        name: "plain".to_owned(),
    };
    let details = session.describe(&object).await.expect("describe");

    // A user column named "oid" shadows that alias; "rowid" is still free.
    assert_eq!(
        details.row_locator.map(|locator| locator.expression),
        Some("rowid".to_owned())
    );
    close(session.as_ref()).await;
}
