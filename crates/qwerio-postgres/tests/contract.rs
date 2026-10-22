//! The engine-neutral contract suite against a disposable PostgreSQL server.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::TestServer;
use qwerio_core::contract::{ContractFixture, FixtureDatabase};
use qwerio_core::driver::DatabaseDriver;
use qwerio_core::resource::CatalogPath;
use qwerio_postgres::PostgresDriver;

struct PostgresFixture {
    server: TestServer,
}

impl PostgresFixture {
    fn new() -> Self {
        Self {
            server: TestServer::from_env(),
        }
    }
}

#[async_trait]
impl ContractFixture for PostgresFixture {
    fn driver(&self) -> Arc<dyn DatabaseDriver> {
        Arc::new(PostgresDriver::new(tokio::runtime::Handle::current()))
    }

    async fn create(&self) -> FixtureDatabase {
        let database = self.server.create_database().await;

        FixtureDatabase {
            connection: self.server.descriptor(&database),
            credentials: self.server.credentials(),
            path: CatalogPath::namespace(Some(database.clone()), "public"),
            database: Some(database),
        }
    }

    async fn destroy(&self, database: FixtureDatabase) {
        if let Some(name) = database.database {
            self.server.drop_database(&name).await;
        }
    }

    fn schema_statements(&self) -> Vec<String> {
        [
            "CREATE TABLE items (id integer PRIMARY KEY, name text, score double precision, big bigint)",
            "CREATE TABLE orders (year integer NOT NULL, number integer NOT NULL, note text, \
             PRIMARY KEY (year, number))",
            "CREATE TABLE lines (id integer PRIMARY KEY, order_no integer, order_year integer, payload bytea, \
             FOREIGN KEY (order_year, order_no) REFERENCES orders (year, number))",
            "CREATE TABLE \"Weird \"\"Name\"\" Table\" (\"Mixed Case\" integer PRIMARY KEY, \"select\" text)",
            "CREATE VIEW item_names AS SELECT id, name FROM items",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    fn blob_literal(&self, hex: &str) -> String {
        format!("'\\x{hex}'::bytea")
    }

    fn slow_query(&self) -> String {
        "SELECT pg_sleep(60)".to_owned()
    }

    fn parameter_query(&self) -> String {
        "SELECT $1::integer".to_owned()
    }

    fn series_query(&self, count: u64) -> String {
        format!("SELECT n FROM generate_series(1, {count}) AS n")
    }
}

qwerio_core::contract_tests!(PostgresFixture::new());

#[tokio::test(flavor = "multi_thread")]
async fn manages_users_roles_and_permissions() {
    qwerio_core::contract::with_database(
        &PostgresFixture::new(),
        qwerio_core::contract::manages_users_roles_and_permissions,
    )
    .await;
}
