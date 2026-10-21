//! Tests connect only to the disposable server provided by with-test-mysql.sh.

#![allow(dead_code)]

use async_trait::async_trait;
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, OptsBuilder};
use qwerio_core::connection::{
    ConnectionDescriptor, ConnectionOptions, Credentials, EngineKind, MysqlOptions, TlsMode,
};
use qwerio_core::contract::{ContractFixture, FixtureDatabase};
use qwerio_core::driver::DatabaseDriver;
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::CatalogPath;
use qwerio_mysql::MysqlDriver;
use std::sync::Arc;

fn variable(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is not set; use scripts/with-test-mysql.sh"))
}

pub struct MysqlFixture;

impl MysqlFixture {
    pub fn engine(&self) -> EngineKind {
        match variable("QWERIO_TEST_MYSQL_ENGINE").as_str() {
            "mysql" => EngineKind::Mysql,
            "mariadb" => EngineKind::Mariadb,
            _ => panic!("Unknown test engine"),
        }
    }

    pub fn options(&self, database: &str) -> MysqlOptions {
        MysqlOptions {
            host: variable("QWERIO_TEST_MYSQL_HOST"),
            port: variable("QWERIO_TEST_MYSQL_PORT").parse().expect("port"),
            user: "root".to_owned(),
            database: database.to_owned(),
            tls: TlsMode::VerifyFull {
                ca_file: Some(variable("QWERIO_TEST_MYSQL_CA").into()),
            },
            connect_timeout: None,
        }
    }

    pub fn descriptor(&self, database: &str) -> ConnectionDescriptor {
        let options = self.options(database);
        ConnectionDescriptor {
            id: ConnectionId::new(),
            name: "test".to_owned(),
            options: if self.engine() == EngineKind::Mariadb {
                ConnectionOptions::Mariadb(options)
            } else {
                ConnectionOptions::Mysql(options)
            },
        }
    }

    pub fn credentials(&self) -> Credentials {
        Credentials::password(variable("QWERIO_TEST_MYSQL_PASSWORD"))
    }

    pub async fn admin(&self) -> Conn {
        Conn::new(
            OptsBuilder::default()
                .ip_or_hostname(variable("QWERIO_TEST_MYSQL_HOST"))
                .tcp_port(variable("QWERIO_TEST_MYSQL_PORT").parse().expect("port"))
                .prefer_socket(false)
                .user(Some("root"))
                .pass(Some(variable("QWERIO_TEST_MYSQL_PASSWORD"))),
        )
        .await
        .expect("admin connection")
    }
}

#[async_trait]
impl ContractFixture for MysqlFixture {
    fn driver(&self) -> Arc<dyn DatabaseDriver> {
        let runtime = tokio::runtime::Handle::current();
        Arc::new(if self.engine() == EngineKind::Mariadb {
            MysqlDriver::mariadb(runtime)
        } else {
            MysqlDriver::new(runtime)
        })
    }

    async fn create(&self) -> FixtureDatabase {
        let name = format!("qwerio_test_{}", ConnectionId::new().as_uuid().simple());
        // UUID's public representation uses hyphens, so always quote it.
        let mut admin = self.admin().await;
        admin
            .query_drop(format!(
                "CREATE DATABASE {} CHARACTER SET utf8mb4 COLLATE utf8mb4_bin",
                self.engine().quote_identifier(&name)
            ))
            .await
            .expect("create database");
        admin.disconnect().await.expect("disconnect");
        FixtureDatabase {
            connection: self.descriptor(&name),
            credentials: self.credentials(),
            path: CatalogPath::database(&name),
            database: Some(name),
        }
    }

    async fn destroy(&self, database: FixtureDatabase) {
        let name = database.database.expect("database");
        assert!(name.starts_with("qwerio_test_"));
        let mut admin = self.admin().await;
        admin
            .query_drop(format!(
                "DROP DATABASE {}",
                self.engine().quote_identifier(&name)
            ))
            .await
            .expect("drop database");
        admin.disconnect().await.expect("disconnect");
    }

    fn schema_statements(&self) -> Vec<String> {
        [
            "CREATE TABLE items (id int PRIMARY KEY, name text, score double, big bigint) ENGINE=InnoDB",
            "CREATE TABLE orders (year int NOT NULL, number int NOT NULL, note text, PRIMARY KEY (year, number)) ENGINE=InnoDB",
            "CREATE TABLE `lines` (id int PRIMARY KEY, order_no int, order_year int, payload blob, FOREIGN KEY (order_year, order_no) REFERENCES orders (year, number)) ENGINE=InnoDB",
            "CREATE TABLE `Weird \"Name\" Table` (`Mixed Case` int PRIMARY KEY, `select` text) ENGINE=InnoDB",
            "CREATE VIEW item_names AS SELECT id, name FROM items",
        ].map(str::to_owned).to_vec()
    }

    fn blob_literal(&self, hex: &str) -> String {
        format!("X'{hex}'")
    }

    fn slow_query(&self) -> String {
        "SELECT SLEEP(60)".to_owned()
    }

    fn parameter_query(&self) -> String {
        "SELECT ?".to_owned()
    }

    fn series_query(&self, count: u64) -> String {
        format!(
            "SELECT a.n + b.n * 10 + c.n * 100 + d.n * 1000 + e.n * 10000 + 1 AS n FROM \
            (SELECT 0 n UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9) a \
            CROSS JOIN (SELECT 0 n UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9) b \
            CROSS JOIN (SELECT 0 n UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9) c \
            CROSS JOIN (SELECT 0 n UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9) d \
            CROSS JOIN (SELECT 0 n UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9) e LIMIT {count}"
        )
    }
}
