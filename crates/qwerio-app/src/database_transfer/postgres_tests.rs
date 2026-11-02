//! Run explicitly through scripts/with-test-postgres.sh; never use a shared server.

use std::process::Command;
use std::sync::atomic::AtomicBool;

use qwerio_core::connection::{ConnectionOptions, Credentials, PostgresOptions, TlsMode};
use qwerio_core::ids::ConnectionId;

use super::*;

fn variable(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("Run through scripts/with-test-postgres.sh: missing {name}"))
}

fn sql(database: &str, statement: &str) -> Result<String, String> {
    let output = Command::new("psql")
        .args([
            "--no-psqlrc",
            "--no-password",
            "--set=ON_ERROR_STOP=1",
            "-At",
            "--command",
            statement,
        ])
        .env("PGHOST", variable("QWERIO_TEST_POSTGRES_HOST"))
        .env("PGPORT", variable("QWERIO_TEST_POSTGRES_PORT"))
        .env("PGUSER", variable("QWERIO_TEST_POSTGRES_USER"))
        .env("PGPASSWORD", variable("QWERIO_TEST_POSTGRES_PASSWORD"))
        .env("PGDATABASE", database)
        .env("PGSSLMODE", "disable")
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

struct TestDatabases(Vec<String>);

impl Drop for TestDatabases {
    fn drop(&mut self) {
        for name in &self.0 {
            if let Err(error) = sql("postgres", &format!("DROP DATABASE \"{name}\"")) {
                eprintln!("Could not remove disposable transfer database: {error}");
            }
        }
    }
}

#[test]
#[ignore = "requires scripts/with-test-postgres.sh and native PostgreSQL clients"]
fn native_postgres_archive_and_sql_round_trip() {
    assert_eq!(variable("QWERIO_TEST_POSTGRES_HOST"), "localhost");
    assert_eq!(variable("QWERIO_TEST_POSTGRES_USER"), "qwerio_test");
    let directory = tempfile::tempdir().expect("directory");
    let suffix = ConnectionId::new().as_uuid().simple().to_string();
    let mut databases = TestDatabases(Vec::new());
    for format in [Format::Archive, Format::Sql] {
        let kind = if format == Format::Archive {
            "archive"
        } else {
            "sql"
        };
        let source = format!("qwerio_test_{kind}_{suffix}");
        let destination = format!("qwerio_test_{kind}={suffix}");
        for database in [&source, &destination] {
            sql("postgres", &format!("CREATE DATABASE \"{database}\"")).expect("create database");
            databases.0.push(database.clone());
        }
        sql(
            &source,
            "CREATE TABLE items (name TEXT); INSERT INTO items VALUES ('hello'); GRANT SELECT ON items TO PUBLIC;",
        )
        .expect("seed");
        sql(
            &destination,
            "CREATE TABLE items (name TEXT); INSERT INTO items VALUES ('old data');",
        )
        .expect("existing target");
        let options = ConnectionOptions::Postgres(PostgresOptions {
            host: variable("QWERIO_TEST_POSTGRES_HOST"),
            port: variable("QWERIO_TEST_POSTGRES_PORT").parse().expect("port"),
            user: variable("QWERIO_TEST_POSTGRES_USER"),
            database: "postgres".to_owned(),
            show_all_databases: true,
            tls: TlsMode::VerifyFull {
                ca_file: Some(variable("QWERIO_TEST_POSTGRES_CA").into()),
            },
            connect_timeout: None,
            application_name: None,
        });
        let file = directory.path().join(format!("{kind}.dump"));
        for operation in [Operation::Backup, Operation::Restore] {
            let tool = format.tool(EngineKind::Postgres, operation).expect("tool");
            let request = TransferRequest {
                options: options.clone(),
                credentials: Credentials::password(variable("QWERIO_TEST_POSTGRES_PASSWORD")),
                database: Some(if operation == Operation::Backup {
                    source.clone()
                } else {
                    destination.clone()
                }),
                operation,
                format,
                file: file.clone(),
                restored_file: None,
                tool: Some(tools::detect(tool, None).expect("installed tool").path),
            };
            run(request, &AtomicBool::new(false)).expect("transfer");
        }
        assert_eq!(
            sql(&destination, "SELECT name FROM items").expect("restored row"),
            "hello"
        );
        assert_eq!(
            sql(&destination, "SELECT EXISTS (SELECT 1 FROM information_schema.table_privileges WHERE table_name = 'items' AND grantee = 'PUBLIC' AND privilege_type = 'SELECT')").expect("restored grant"),
            "t"
        );
    }
}
