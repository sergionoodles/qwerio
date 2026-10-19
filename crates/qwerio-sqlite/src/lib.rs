//! SQLite adapter for the Qwerio database contracts.
//!
//! Each session owns one connection on a dedicated worker thread. Existing
//! files open without SQLite's CREATE flag; creating a database file is the
//! separate [`create_database_file`] action. Read-only sessions use the
//! driver's read-only open flags, so writes fail inside SQLite itself.

mod catalog;
mod convert;
mod dialect;
mod error;
mod execute;
mod session;
mod worker;

use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use qwerio_core::capabilities::{CancellationSupport, DatabaseCapabilities};
use qwerio_core::connection::{ConnectionOptions, EngineKind, SqliteOptions};
use qwerio_core::driver::{DatabaseDriver, DatabaseSession, OpenSessionRequest, ValidationIssue};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::filter::FilterOperator;
use qwerio_core::ids::SessionId;
use qwerio_core::session::{AccessMode, ReadOnlyEnforcement, SessionInfo};
use rusqlite::{Connection, OpenFlags};

use crate::error::map_error;
use crate::session::SqliteSession;
use crate::worker::Worker;

/// SQLite's storage classes, offered for new columns.
const COLUMN_TYPES: [&str; 5] = ["TEXT", "INTEGER", "REAL", "NUMERIC", "BLOB"];
/// An INTEGER PRIMARY KEY aliases the rowid, so SQLite assigns its values.
const KEY_COLUMN_TYPE: &str = "INTEGER";

pub struct SqliteDriver {
    capabilities: DatabaseCapabilities,
}

impl SqliteDriver {
    pub fn new() -> Self {
        Self {
            capabilities: DatabaseCapabilities {
                has_databases: false,
                has_namespaces: false,
                access_modes: vec![AccessMode::ReadOnly, AccessMode::ReadWrite],
                read_only_enforcement: ReadOnlyEnforcement::ReadOnlyFile,
                transactional_ddl: true,
                cancellation: CancellationSupport::Interrupt,
                filter_operators: FilterOperator::ALL.to_vec(),
                unicode_case_folding: false,
                errors_abort_transactions: false,
                column_types: COLUMN_TYPES.iter().map(|name| (*name).to_owned()).collect(),
                key_column_type: KEY_COLUMN_TYPE.to_owned(),
            },
        }
    }
}

impl Default for SqliteDriver {
    fn default() -> Self {
        Self::new()
    }
}

/// Initialization data reported by the worker after opening.
struct OpenedInfo {
    foreign_keys_enforced: bool,
    version: String,
}

#[async_trait]
impl DatabaseDriver for SqliteDriver {
    fn engine(&self) -> EngineKind {
        EngineKind::Sqlite
    }

    fn capabilities(&self) -> &DatabaseCapabilities {
        &self.capabilities
    }

    fn validate(&self, options: &ConnectionOptions) -> Vec<ValidationIssue> {
        let ConnectionOptions::Sqlite(options) = options else {
            return vec![ValidationIssue {
                field: "engine",
                message: "These options are not for SQLite".to_owned(),
            }];
        };

        if options.path.as_os_str().is_empty() {
            return vec![ValidationIssue {
                field: "path",
                message: "Choose a database file".to_owned(),
            }];
        }

        if !options.path.is_absolute() {
            return vec![ValidationIssue {
                field: "path",
                message: "Use an absolute file path".to_owned(),
            }];
        }

        Vec::new()
    }

    async fn open(&self, request: OpenSessionRequest) -> DbResult<Arc<dyn DatabaseSession>> {
        let ConnectionOptions::Sqlite(options) = request.connection.options.clone() else {
            return Err(DatabaseError::invalid_request(
                "These options are not for SQLite",
            ));
        };

        if let Some(issue) = self.validate(&request.connection.options).first() {
            return Err(DatabaseError::invalid_request(issue.message.clone()));
        }

        if request.database.is_some() {
            return Err(DatabaseError::invalid_request(
                "SQLite files contain a single database",
            ));
        }

        let access = request.access;
        let thread_name = format!("qwerio-sqlite-{}", request.connection.name);
        let (worker, opened) =
            Worker::spawn(thread_name, move || open_connection(&options, access)).await?;

        let enforcement = match access {
            AccessMode::ReadOnly => ReadOnlyEnforcement::ReadOnlyFile,
            AccessMode::ReadWrite => ReadOnlyEnforcement::NotEnforced,
        };

        let info = SessionInfo {
            id: SessionId::new(),
            connection: request.connection.id,
            engine: EngineKind::Sqlite,
            database: None,
            purpose: request.purpose,
            requested_access: access,
            enforcement,
            server_version: Some(opened.version),
            foreign_keys_enforced: Some(opened.foreign_keys_enforced),
        };

        Ok(Arc::new(SqliteSession::new(info, worker)))
    }
}

fn open_flags(access: AccessMode) -> OpenFlags {
    // Never SQLITE_OPEN_CREATE, and no URI interpretation of the path.
    let mode = match access {
        AccessMode::ReadOnly => OpenFlags::SQLITE_OPEN_READ_ONLY,
        AccessMode::ReadWrite => OpenFlags::SQLITE_OPEN_READ_WRITE,
    };

    mode | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_EXRESCODE
}

fn open_connection(
    options: &SqliteOptions,
    access: AccessMode,
) -> DbResult<(Connection, OpenedInfo)> {
    if !options.path.is_file() {
        return Err(DatabaseError::new(
            ErrorKind::NotFound,
            format!("No database file at {}", options.path.display()),
        ));
    }

    let connection =
        Connection::open_with_flags(&options.path, open_flags(access)).map_err(map_error)?;
    connection
        .busy_timeout(options.busy_timeout)
        .map_err(map_error)?;

    let foreign_keys_pragma = if options.enforce_foreign_keys {
        "PRAGMA foreign_keys = ON"
    } else {
        "PRAGMA foreign_keys = OFF"
    };
    connection
        .execute_batch(foreign_keys_pragma)
        .map_err(map_error)?;

    // Reading the schema fails here for files that are not SQLite databases.
    connection
        .query_row("SELECT count(*) FROM main.sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(map_error)?;

    let foreign_keys_enforced = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, bool>(0))
        .map_err(map_error)?;

    let info = OpenedInfo {
        foreign_keys_enforced,
        version: rusqlite::version().to_owned(),
    };

    Ok((connection, info))
}

/// Creates a new, empty database file. Fails if anything exists at `path`.
/// This is the only way the adapter creates files.
pub fn create_database_file(path: &Path) -> DbResult<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            let kind = match error.kind() {
                std::io::ErrorKind::AlreadyExists => ErrorKind::InvalidRequest,
                std::io::ErrorKind::PermissionDenied => ErrorKind::Permission,
                _ => ErrorKind::Other,
            };

            DatabaseError::new(
                kind,
                format!("Could not create {}: {error}", path.display()),
            )
        })?;

    // An empty file is a valid empty database; initialize its header so other
    // tools recognize it.
    let connection =
        Connection::open_with_flags(path, open_flags(AccessMode::ReadWrite)).map_err(map_error)?;
    connection
        .execute_batch("PRAGMA user_version = 0; VACUUM;")
        .map_err(map_error)
}
