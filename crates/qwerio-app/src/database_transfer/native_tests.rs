use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use qwerio_core::connection::{ConnectionOptions, Credentials, PostgresOptions, TlsMode};

use super::*;

// Fresh executable fixtures can produce ETXTBSY during parallel process spawns.
static CLIENT_FIXTURE_LOCK: Mutex<()> = Mutex::new(());

fn executable(directory: &Path, body: &str) -> PathBuf {
    let path = directory.join("pg_dump");
    std::fs::write(&path, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'pg_dump (PostgreSQL) 18.0\\n'; exit 0; fi\n{body}\n")).expect("client fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).expect("executable");
    path
}

fn request(directory: &Path, tool: PathBuf) -> TransferRequest {
    TransferRequest {
        options: ConnectionOptions::Postgres(PostgresOptions {
            host: "localhost".to_owned(),
            port: 5432,
            user: "user".to_owned(),
            database: "default_database".to_owned(),
            show_all_databases: true,
            tls: TlsMode::VerifyFull { ca_file: None },
            connect_timeout: None,
            application_name: None,
        }),
        credentials: Credentials::password("example-password"),
        database: Some("literal=name".to_owned()),
        operation: Operation::Backup,
        format: Format::Archive,
        file: directory.join("backup.dump"),
        restored_file: None,
        tool: Some(tool),
    }
}

#[test]
fn streams_backup_output_with_literal_database_and_private_credentials() {
    let _guard = CLIENT_FIXTURE_LOCK.lock().expect("client fixture lock");
    let directory = tempfile::tempdir().expect("directory");
    let tool = executable(
        directory.path(),
        r#"
test "$PGDATABASE" = 'literal=name' || exit 2
test "$PGSSLMODE" = verify-full || exit 3
test -f "$PGPASSFILE" || exit 4
test -z "$PGPASSWORD" || exit 5
printf 'backup contents'
"#,
    );
    let request = request(directory.path(), tool);
    let path = run(request, &AtomicBool::new(false)).expect("backup");
    assert_eq!(
        std::fs::read_to_string(&path).expect("output"),
        "backup contents"
    );
    assert_eq!(
        std::fs::metadata(path)
            .expect("permissions")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::read_dir(directory.path()).expect("files").count(),
        2
    );
}

#[test]
fn failed_clients_leave_no_backup_and_redact_passwords() {
    let _guard = CLIENT_FIXTURE_LOCK.lock().expect("client fixture lock");
    let directory = tempfile::tempdir().expect("directory");
    let tool = executable(
        directory.path(),
        "printf partial; printf 'rejected example-password' >&2; exit 1",
    );
    let error = run(request(directory.path(), tool), &AtomicBool::new(false)).expect_err("failure");
    assert!(error.contains("rejected [redacted]"));
    assert!(!error.contains("example-password"));
    assert!(!directory.path().join("backup.dump").exists());
    assert_eq!(
        std::fs::read_dir(directory.path()).expect("files").count(),
        1
    );
}

#[test]
fn cancellation_stops_the_client_and_removes_partial_output() {
    let _guard = CLIENT_FIXTURE_LOCK.lock().expect("client fixture lock");
    let directory = tempfile::tempdir().expect("directory");
    let tool = executable(directory.path(), "printf partial; exec sleep 5");
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let request = request(directory.path(), tool);
    let worker = std::thread::spawn(move || run(request, &worker_cancelled));
    std::thread::sleep(Duration::from_millis(200));
    cancelled.store(true, Ordering::Relaxed);
    assert!(
        worker
            .join()
            .expect("worker")
            .expect_err("cancelled")
            .contains("Cancelled")
    );
    assert!(!directory.path().join("backup.dump").exists());
    assert_eq!(
        std::fs::read_dir(directory.path()).expect("files").count(),
        1
    );
}

#[test]
fn missing_or_incorrect_tools_explain_what_is_required() {
    let _guard = CLIENT_FIXTURE_LOCK.lock().expect("client fixture lock");
    let directory = tempfile::tempdir().expect("directory");
    assert!(
        tools::detect(tools::NativeTool::Psql, Some(directory.path()))
            .expect_err("missing")
            .contains("psql was not found")
    );
    let tool = executable(directory.path(), "exit 0");
    assert!(
        tools::check(tools::NativeTool::Psql, &tool)
            .expect_err("wrong tool")
            .contains("not psql")
    );
    assert!(
        tools::detect(tools::NativeTool::PgDump, Some(Path::new("relative")))
            .expect_err("relative directory")
            .contains("absolute path")
    );
}
