//! Typed connection descriptors.
//!
//! Descriptors hold machine-local endpoint information. Shared workspace
//! manifests only store a [`ConnectionId`] and an engine; secrets are passed
//! separately through [`Credentials`] when a session opens.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ids::ConnectionId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    Postgres,
    Sqlite,
    Mysql,
    Mariadb,
}

impl EngineKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::Sqlite => "SQLite",
            Self::Mysql => "MySQL",
            Self::Mariadb => "MariaDB",
        }
    }

    pub fn quote_identifier(self, identifier: &str) -> String {
        if matches!(self, Self::Mysql | Self::Mariadb) {
            return format!("`{}`", identifier.replace('`', "``"));
        }

        crate::sql::quote_identifier_ansi(identifier)
    }
}

impl fmt::Display for EngineKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.display_name())
    }
}

/// A connection alias bound to concrete, machine-local options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnectionDescriptor {
    pub id: ConnectionId,
    pub name: String,
    pub options: ConnectionOptions,
}

impl ConnectionDescriptor {
    pub fn engine(&self) -> EngineKind {
        self.options.engine()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "engine", rename_all = "lowercase")]
pub enum ConnectionOptions {
    Postgres(PostgresOptions),
    Sqlite(SqliteOptions),
    Mysql(MysqlOptions),
    Mariadb(MysqlOptions),
}

impl ConnectionOptions {
    pub fn engine(&self) -> EngineKind {
        match self {
            Self::Postgres(_) => EngineKind::Postgres,
            Self::Sqlite(_) => EngineKind::Sqlite,
            Self::Mysql(_) => EngineKind::Mysql,
            Self::Mariadb(_) => EngineKind::Mariadb,
        }
    }
}

fn show_all_databases_by_default() -> bool {
    true
}

pub const DEFAULT_MYSQL_PORT: u16 = 3306;

/// Shared endpoint options for MySQL and MariaDB. A socket is an absolute file path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MysqlOptions {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    /// Whether the catalog lists every database or only the configured database.
    #[serde(default = "show_all_databases_by_default")]
    pub show_all_databases: bool,
    pub tls: TlsMode,
    #[serde(default, with = "duration_seconds")]
    pub connect_timeout: Option<Duration>,
}

pub const DEFAULT_POSTGRES_PORT: u16 = 5432;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresOptions {
    /// Host name, IP address, or an absolute Unix socket directory.
    pub host: String,
    pub port: u16,
    pub user: String,
    /// The database opened first. Other databases on the server open their own sessions.
    pub database: String,
    /// Whether the catalog lists every database or only the configured database.
    #[serde(default = "show_all_databases_by_default")]
    pub show_all_databases: bool,
    pub tls: TlsMode,
    #[serde(default, with = "duration_seconds")]
    pub connect_timeout: Option<Duration>,
    pub application_name: Option<String>,
}

impl PostgresOptions {
    pub fn is_unix_socket(&self) -> bool {
        self.host.starts_with('/')
    }
}

/// TLS policy. Encrypted connections always validate the server certificate
/// and host name; there is no "encrypt without verifying" mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TlsMode {
    /// Plain-text connection. Intended for local sockets and development servers.
    Disable,
    /// Require TLS, verifying the certificate chain against the platform trust
    /// store or the given CA bundle, and the host name.
    VerifyFull { ca_file: Option<PathBuf> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqliteOptions {
    /// An existing database file. Sessions never create it implicitly.
    pub path: PathBuf,
    /// Applies `PRAGMA foreign_keys` on every connection of this alias.
    pub enforce_foreign_keys: bool,
    #[serde(with = "duration_millis")]
    pub busy_timeout: Duration,
}

pub const DEFAULT_SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

impl SqliteOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            enforce_foreign_keys: true,
            busy_timeout: DEFAULT_SQLITE_BUSY_TIMEOUT,
        }
    }
}

/// A secret that never appears in `Debug` output or serialized form.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(********)")
    }
}

/// Credentials resolved from the platform credential store for one open call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credentials {
    pub password: Option<Secret>,
}

impl Credentials {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn password(password: impl Into<String>) -> Self {
        Self {
            password: Some(Secret::new(password)),
        }
    }
}

mod duration_seconds {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(duration) => serializer.serialize_some(&duration.as_secs()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        Ok(Option::<u64>::deserialize(deserializer)?.map(Duration::from_secs))
    }
}

mod duration_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        let millis = u64::try_from(value.as_millis()).unwrap_or(u64::MAX);
        serializer.serialize_u64(millis)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_millis(u64::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_output_is_redacted() {
        let credentials = Credentials::password("hunter2");

        assert!(!format!("{credentials:?}").contains("hunter2"));
    }
}
