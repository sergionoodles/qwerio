//! Access to the disposable PostgreSQL server started by
//! `scripts/with-test-postgres.sh`. Tests only create and drop databases whose
//! names start with `qwerio_test_`.

#![allow(dead_code)]

use std::path::PathBuf;

use qwerio_core::connection::{
    ConnectionDescriptor, ConnectionOptions, Credentials, PostgresOptions, TlsMode,
};
use qwerio_core::ids::ConnectionId;
use tokio_postgres::{Client, NoTls};

pub struct TestServer {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub ca_file: PathBuf,
}

fn variable(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is not set; run PostgreSQL tests through scripts/with-test-postgres.sh")
    })
}

impl TestServer {
    pub fn from_env() -> Self {
        Self {
            host: variable("QWERIO_TEST_POSTGRES_HOST"),
            port: variable("QWERIO_TEST_POSTGRES_PORT")
                .parse()
                .expect("numeric port"),
            user: variable("QWERIO_TEST_POSTGRES_USER"),
            password: variable("QWERIO_TEST_POSTGRES_PASSWORD"),
            ca_file: PathBuf::from(variable("QWERIO_TEST_POSTGRES_CA")),
        }
    }

    pub fn options(&self, database: &str) -> PostgresOptions {
        PostgresOptions {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            database: database.to_owned(),
            tls: TlsMode::VerifyFull {
                ca_file: Some(self.ca_file.clone()),
            },
            connect_timeout: None,
            application_name: None,
        }
    }

    pub fn descriptor(&self, database: &str) -> ConnectionDescriptor {
        ConnectionDescriptor {
            id: ConnectionId::new(),
            name: "test".to_owned(),
            options: ConnectionOptions::Postgres(self.options(database)),
        }
    }

    pub fn credentials(&self) -> Credentials {
        Credentials::password(self.password.clone())
    }

    /// An administrative connection to the `postgres` database.
    pub async fn admin(&self) -> Client {
        let config = format!(
            "host={} port={} user={} password={} dbname=postgres",
            self.host, self.port, self.user, self.password
        );
        let (client, connection) = tokio_postgres::connect(&config, NoTls)
            .await
            .expect("admin connection");
        tokio::spawn(connection);
        client
    }

    pub async fn create_database(&self) -> String {
        let name = format!("qwerio_test_{}", ConnectionId::new().as_uuid().simple());
        self.admin()
            .await
            .batch_execute(&format!("CREATE DATABASE {name}"))
            .await
            .expect("create test database");
        name
    }

    pub async fn drop_database(&self, name: &str) {
        assert!(name.starts_with("qwerio_test_"), "refusing to drop {name}");
        self.admin()
            .await
            .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
            .await
            .expect("drop test database");
    }
}
