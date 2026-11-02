//! Whole-database backup and restore. All entry points are blocking and must
//! run off the UI thread. Native clients connect independently of UI sessions.

mod files;
mod native;
mod process;
mod sqlite;
pub mod tools;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use qwerio_core::connection::{ConnectionOptions, Credentials, EngineKind};

use tools::NativeTool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Backup,
    Restore,
}

impl Operation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Backup => "Backup Database",
            Self::Restore => "Restore Database",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Archive,
    Sql,
}

impl Format {
    pub fn tool(self, engine: EngineKind, operation: Operation) -> Option<NativeTool> {
        match (engine, operation, self) {
            (EngineKind::Sqlite, _, _) => None,
            (EngineKind::Postgres, Operation::Backup, _) => Some(NativeTool::PgDump),
            (EngineKind::Postgres, Operation::Restore, Self::Archive) => {
                Some(NativeTool::PgRestore)
            }
            (EngineKind::Postgres, Operation::Restore, Self::Sql) => Some(NativeTool::Psql),
            (EngineKind::Mysql, Operation::Backup, _) => Some(NativeTool::MysqlDump),
            (EngineKind::Mysql, Operation::Restore, _) => Some(NativeTool::Mysql),
            (EngineKind::Mariadb, Operation::Backup, _) => Some(NativeTool::MariadbDump),
            (EngineKind::Mariadb, Operation::Restore, _) => Some(NativeTool::Mariadb),
        }
    }
}

pub struct TransferRequest {
    pub options: ConnectionOptions,
    pub credentials: Credentials,
    pub database: Option<String>,
    pub operation: Operation,
    pub format: Format,
    /// Backup destination or restore source.
    pub file: PathBuf,
    /// SQLite restores always create a separate database file.
    pub restored_file: Option<PathBuf>,
    pub tool: Option<PathBuf>,
}

pub fn run(request: TransferRequest, cancelled: &AtomicBool) -> Result<PathBuf, String> {
    if cancelled.load(Ordering::Relaxed) {
        return Err("Cancelled".to_owned());
    }
    if !request.file.is_absolute() {
        return Err("Choose an absolute file path".to_owned());
    }
    let result = match &request.options {
        ConnectionOptions::Sqlite(options) => sqlite::run(&request, options, cancelled),
        _ => native::run(&request, cancelled),
    };
    // A native client may repeat connection details in its diagnostics.
    result.map_err(|message| match &request.credentials.password {
        Some(password) if !password.expose().is_empty() => {
            message.replace(password.expose(), "[redacted]")
        }
        _ => message,
    })
}

#[cfg(all(test, unix))]
mod native_tests;
#[cfg(test)]
mod postgres_tests;
#[cfg(test)]
mod tests;
