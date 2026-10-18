//! The engine-neutral contract suite against temporary SQLite files.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use qwerio_core::connection::{
    ConnectionDescriptor, ConnectionOptions, Credentials, SqliteOptions,
};
use qwerio_core::contract::{ContractFixture, FixtureDatabase};
use qwerio_core::driver::DatabaseDriver;
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::CatalogPath;
use qwerio_sqlite::{SqliteDriver, create_database_file};
use tempfile::TempDir;

struct SqliteFixture {
    directory: TempDir,
}

impl SqliteFixture {
    fn new() -> Self {
        Self {
            directory: TempDir::new().expect("temporary directory"),
        }
    }

    fn database_path(&self) -> PathBuf {
        self.directory
            .path()
            .join(format!("{}.sqlite", ConnectionId::new()))
    }
}

#[async_trait]
impl ContractFixture for SqliteFixture {
    fn driver(&self) -> Arc<dyn DatabaseDriver> {
        Arc::new(SqliteDriver::new())
    }

    async fn create(&self) -> FixtureDatabase {
        let path = self.database_path();
        create_database_file(&path).expect("create database file");

        FixtureDatabase {
            connection: ConnectionDescriptor {
                id: ConnectionId::new(),
                name: "contract".to_owned(),
                options: ConnectionOptions::Sqlite(SqliteOptions::new(path)),
            },
            credentials: Credentials::none(),
            path: CatalogPath::root(),
            database: None,
        }
    }

    async fn destroy(&self, database: FixtureDatabase) {
        if let ConnectionOptions::Sqlite(options) = database.connection.options {
            std::fs::remove_file(options.path).expect("remove database file");
        }
    }

    fn schema_statements(&self) -> Vec<String> {
        [
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, score REAL, big INTEGER)",
            "CREATE TABLE orders (year INTEGER NOT NULL, number INTEGER NOT NULL, note TEXT, \
             PRIMARY KEY (year, number))",
            "CREATE TABLE lines (id INTEGER PRIMARY KEY, order_no INTEGER, order_year INTEGER, payload BLOB, \
             FOREIGN KEY (order_year, order_no) REFERENCES orders (year, number))",
            "CREATE TABLE \"Weird \"\"Name\"\" Table\" (\"Mixed Case\" INTEGER PRIMARY KEY, \"select\" TEXT)",
            "CREATE VIEW item_names AS SELECT id, name FROM items",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    fn blob_literal(&self, hex: &str) -> String {
        format!("X'{hex}'")
    }

    fn slow_query(&self) -> String {
        "WITH RECURSIVE counter(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM counter) \
         SELECT count(*) FROM counter"
            .to_owned()
    }

    fn parameter_query(&self) -> String {
        "SELECT ?1".to_owned()
    }

    fn series_query(&self, count: u64) -> String {
        format!(
            "WITH RECURSIVE series(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM series WHERE n < {count}) \
             SELECT n FROM series"
        )
    }
}

qwerio_core::contract_tests!(SqliteFixture::new());
