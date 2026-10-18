//! Machine-local application state in Qwerio's own SQLite database: endpoint
//! bindings for connection aliases, recent workspaces, saved tabs, unsaved
//! drafts, query history, and settings. Never shared with a workspace and
//! never opened as a user database.
//!
//! Calls are blocking; run them off the UI thread.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use qwerio_core::connection::ConnectionOptions;
use qwerio_core::ids::ConnectionId;
use qwerio_workspace::document::Fingerprint;
use qwerio_workspace::manifest::{DocumentId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::credentials::CredentialRef;
use crate::error::{AppError, AppResult};

/// Schema migrations, applied in order. The index + 1 is the schema version.
const MIGRATIONS: [&str; 1] = ["
    CREATE TABLE connection_bindings (
        workspace_id TEXT NOT NULL,
        connection_id TEXT NOT NULL,
        options_json TEXT NOT NULL,
        credential_ref TEXT,
        PRIMARY KEY (workspace_id, connection_id)
    );
    CREATE TABLE recent_workspaces (
        path TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        opened_at INTEGER NOT NULL
    );
    CREATE TABLE workspace_ui_state (
        workspace_id TEXT PRIMARY KEY,
        state_json TEXT NOT NULL
    );
    CREATE TABLE drafts (
        workspace_id TEXT NOT NULL,
        document_id TEXT NOT NULL,
        text TEXT NOT NULL,
        base_fingerprint_json TEXT,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (workspace_id, document_id)
    );
    CREATE TABLE query_history (
        id INTEGER PRIMARY KEY,
        workspace_id TEXT NOT NULL,
        connection_id TEXT NOT NULL,
        database_name TEXT,
        sql TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        duration_ms INTEGER NOT NULL,
        outcome TEXT NOT NULL,
        rows INTEGER
    );
    CREATE INDEX query_history_by_workspace ON query_history (workspace_id, started_at);
    CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value_json TEXT NOT NULL
    );
"];

pub struct LocalState {
    connection: Mutex<Connection>,
}

/// A connection alias bound to an endpoint or file on this machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnectionBinding {
    pub options: ConnectionOptions,
    pub credential: Option<CredentialRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentWorkspace {
    pub path: PathBuf,
    pub name: String,
    pub opened_at: SystemTime,
}

/// An unsaved document buffer and the file version it was based on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub document: DocumentId,
    pub text: String,
    pub base: Option<Fingerprint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl QueryOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "succeeded" => Self::Succeeded,
            "cancelled" => Self::Cancelled,
            _ => Self::Failed,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub connection: ConnectionId,
    pub database: Option<String>,
    pub sql: String,
    pub started_at: SystemTime,
    pub duration: Duration,
    pub outcome: QueryOutcome,
    pub rows: Option<u64>,
}

/// How much query history to keep. SQL text can contain sensitive data, so
/// history is bounded and can be disabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRetention {
    pub enabled: bool,
    pub max_entries: u32,
    pub max_age_days: u32,
}

impl Default for HistoryRetention {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: 1_000,
            max_age_days: 30,
        }
    }
}

const SECONDS_PER_DAY: u64 = 86_400;

fn unix_seconds(time: SystemTime) -> i64 {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

fn from_unix_seconds(seconds: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).unwrap_or(0))
}

impl LocalState {
    /// Opens or creates the state database. Its directory must exist.
    pub fn open(path: &Path) -> AppResult<Self> {
        let connection = Connection::open(path)?;
        Self::initialize(connection)
    }

    pub fn in_memory() -> AppResult<Self> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(connection: Connection) -> AppResult<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        migrate(&connection)?;

        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn connection(&self) -> MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    // --- Connection bindings -------------------------------------------------

    pub fn binding(
        &self,
        workspace: WorkspaceId,
        connection: ConnectionId,
    ) -> AppResult<Option<ConnectionBinding>> {
        let row: Option<(String, Option<String>)> = self
            .connection()
            .query_row(
                "SELECT options_json, credential_ref FROM connection_bindings \
                 WHERE workspace_id = ?1 AND connection_id = ?2",
                params![workspace.to_string(), connection.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        row.map(|(options_json, credential)| {
            Ok(ConnectionBinding {
                options: serde_json::from_str(&options_json)?,
                credential: credential
                    .map(|reference| serde_json::from_str(&reference))
                    .transpose()?,
            })
        })
        .transpose()
    }

    pub fn bound_connections(&self, workspace: WorkspaceId) -> AppResult<Vec<ConnectionId>> {
        let connection = self.connection();
        let mut statement = connection
            .prepare("SELECT connection_id FROM connection_bindings WHERE workspace_id = ?1")?;
        let identifiers = statement
            .query_map(params![workspace.to_string()], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;

        identifiers
            .into_iter()
            .map(|identifier| {
                identifier
                    .parse()
                    .map(ConnectionId::from_uuid)
                    .map_err(|_| {
                        AppError::LocalState(format!("invalid connection id {identifier}"))
                    })
            })
            .collect()
    }

    pub fn save_binding(
        &self,
        workspace: WorkspaceId,
        connection: ConnectionId,
        binding: &ConnectionBinding,
    ) -> AppResult<()> {
        let options_json = serde_json::to_string(&binding.options)?;
        let credential = binding
            .credential
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        self.connection().execute(
            "INSERT INTO connection_bindings (workspace_id, connection_id, options_json, credential_ref) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (workspace_id, connection_id) DO UPDATE \
             SET options_json = excluded.options_json, credential_ref = excluded.credential_ref",
            params![workspace.to_string(), connection.to_string(), options_json, credential],
        )?;

        Ok(())
    }

    pub fn remove_binding(
        &self,
        workspace: WorkspaceId,
        connection: ConnectionId,
    ) -> AppResult<()> {
        self.connection().execute(
            "DELETE FROM connection_bindings WHERE workspace_id = ?1 AND connection_id = ?2",
            params![workspace.to_string(), connection.to_string()],
        )?;

        Ok(())
    }

    // --- Recent workspaces ---------------------------------------------------

    pub fn record_recent_workspace(
        &self,
        path: &Path,
        name: &str,
        opened_at: SystemTime,
    ) -> AppResult<()> {
        self.connection().execute(
            "INSERT INTO recent_workspaces (path, name, opened_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT (path) DO UPDATE SET name = excluded.name, opened_at = excluded.opened_at",
            params![path.to_string_lossy(), name, unix_seconds(opened_at)],
        )?;

        Ok(())
    }

    pub fn recent_workspaces(&self, limit: u32) -> AppResult<Vec<RecentWorkspace>> {
        let connection = self.connection();
        let mut statement = connection
            .prepare("SELECT path, name, opened_at FROM recent_workspaces ORDER BY opened_at DESC, path LIMIT ?1")?;

        let workspaces = statement
            .query_map(params![limit], |row| {
                Ok(RecentWorkspace {
                    path: PathBuf::from(row.get::<_, String>(0)?),
                    name: row.get(1)?,
                    opened_at: from_unix_seconds(row.get(2)?),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(workspaces)
    }

    pub fn forget_recent_workspace(&self, path: &Path) -> AppResult<()> {
        self.connection().execute(
            "DELETE FROM recent_workspaces WHERE path = ?1",
            params![path.to_string_lossy()],
        )?;

        Ok(())
    }

    // --- Per-workspace UI state ------------------------------------------------

    pub fn save_ui_state<T: Serialize>(&self, workspace: WorkspaceId, state: &T) -> AppResult<()> {
        let state_json = serde_json::to_string(state)?;

        self.connection().execute(
            "INSERT INTO workspace_ui_state (workspace_id, state_json) VALUES (?1, ?2) \
             ON CONFLICT (workspace_id) DO UPDATE SET state_json = excluded.state_json",
            params![workspace.to_string(), state_json],
        )?;

        Ok(())
    }

    /// The saved state, or `None` when absent or written in an incompatible
    /// shape (the caller then starts fresh).
    pub fn ui_state<T: for<'de> Deserialize<'de>>(
        &self,
        workspace: WorkspaceId,
    ) -> AppResult<Option<T>> {
        let state_json: Option<String> = self
            .connection()
            .query_row(
                "SELECT state_json FROM workspace_ui_state WHERE workspace_id = ?1",
                params![workspace.to_string()],
                |row| row.get(0),
            )
            .optional()?;

        Ok(state_json.and_then(|json| serde_json::from_str(&json).ok()))
    }

    // --- Drafts --------------------------------------------------------------

    pub fn save_draft(
        &self,
        workspace: WorkspaceId,
        draft: &Draft,
        now: SystemTime,
    ) -> AppResult<()> {
        let base = draft.base.as_ref().map(serde_json::to_string).transpose()?;

        self.connection().execute(
            "INSERT INTO drafts (workspace_id, document_id, text, base_fingerprint_json, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (workspace_id, document_id) DO UPDATE \
             SET text = excluded.text, base_fingerprint_json = excluded.base_fingerprint_json, \
                 updated_at = excluded.updated_at",
            params![
                workspace.to_string(),
                draft.document.to_string(),
                draft.text,
                base,
                unix_seconds(now)
            ],
        )?;

        Ok(())
    }

    pub fn drafts(&self, workspace: WorkspaceId) -> AppResult<Vec<Draft>> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT document_id, text, base_fingerprint_json FROM drafts WHERE workspace_id = ?1 ORDER BY updated_at",
        )?;

        let rows = statement
            .query_map(params![workspace.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        rows.into_iter()
            .map(|(document, text, base)| {
                Ok(Draft {
                    document: serde_json::from_value(serde_json::Value::String(document))?,
                    text,
                    base: base.map(|json| serde_json::from_str(&json)).transpose()?,
                })
            })
            .collect()
    }

    pub fn discard_draft(&self, workspace: WorkspaceId, document: DocumentId) -> AppResult<()> {
        self.connection().execute(
            "DELETE FROM drafts WHERE workspace_id = ?1 AND document_id = ?2",
            params![workspace.to_string(), document.to_string()],
        )?;

        Ok(())
    }

    // --- Query history -------------------------------------------------------

    pub fn history_retention(&self) -> AppResult<HistoryRetention> {
        Ok(self.setting("history_retention")?.unwrap_or_default())
    }

    pub fn set_history_retention(
        &self,
        retention: HistoryRetention,
        now: SystemTime,
    ) -> AppResult<()> {
        self.set_setting("history_retention", &retention)?;

        if !retention.enabled {
            return self.clear_history(None);
        }

        self.prune_history(retention, now)
    }

    /// Records a finished query unless history is disabled, then applies the
    /// retention limits.
    pub fn record_history(
        &self,
        workspace: WorkspaceId,
        entry: &HistoryEntry,
        now: SystemTime,
    ) -> AppResult<()> {
        let retention = self.history_retention()?;

        if !retention.enabled {
            return Ok(());
        }

        let duration_ms = i64::try_from(entry.duration.as_millis()).unwrap_or(i64::MAX);
        let rows = entry
            .rows
            .map(|rows| i64::try_from(rows).unwrap_or(i64::MAX));

        self.connection().execute(
            "INSERT INTO query_history \
             (workspace_id, connection_id, database_name, sql, started_at, duration_ms, outcome, rows) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                workspace.to_string(),
                entry.connection.to_string(),
                entry.database,
                entry.sql,
                unix_seconds(entry.started_at),
                duration_ms,
                entry.outcome.as_str(),
                rows
            ],
        )?;

        self.prune_history(retention, now)
    }

    fn prune_history(&self, retention: HistoryRetention, now: SystemTime) -> AppResult<()> {
        let oldest_kept =
            unix_seconds(now) - i64::from(retention.max_age_days) * SECONDS_PER_DAY as i64;
        let connection = self.connection();

        connection.execute(
            "DELETE FROM query_history WHERE started_at < ?1",
            params![oldest_kept],
        )?;
        connection.execute(
            "DELETE FROM query_history WHERE id NOT IN \
             (SELECT id FROM query_history ORDER BY started_at DESC, id DESC LIMIT ?1)",
            params![retention.max_entries],
        )?;

        Ok(())
    }

    pub fn history(&self, workspace: WorkspaceId, limit: u32) -> AppResult<Vec<HistoryEntry>> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT connection_id, database_name, sql, started_at, duration_ms, outcome, rows \
             FROM query_history WHERE workspace_id = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2",
        )?;

        let rows = statement
            .query_map(params![workspace.to_string(), limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        rows.into_iter()
            .map(
                |(connection, database, sql, started_at, duration_ms, outcome, rows)| {
                    Ok(HistoryEntry {
                        connection: serde_json::from_value(serde_json::Value::String(connection))?,
                        database,
                        sql,
                        started_at: from_unix_seconds(started_at),
                        duration: Duration::from_millis(u64::try_from(duration_ms).unwrap_or(0)),
                        outcome: QueryOutcome::parse(&outcome),
                        rows: rows.map(|rows| u64::try_from(rows).unwrap_or(0)),
                    })
                },
            )
            .collect()
    }

    /// Deletes history for one workspace, or all history.
    pub fn clear_history(&self, workspace: Option<WorkspaceId>) -> AppResult<()> {
        let connection = self.connection();

        match workspace {
            Some(workspace) => connection.execute(
                "DELETE FROM query_history WHERE workspace_id = ?1",
                params![workspace.to_string()],
            )?,
            None => connection.execute("DELETE FROM query_history", [])?,
        };

        Ok(())
    }

    // --- Settings ------------------------------------------------------------

    pub fn setting<T: for<'de> Deserialize<'de>>(&self, key: &str) -> AppResult<Option<T>> {
        let value: Option<String> = self
            .connection()
            .query_row(
                "SELECT value_json FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;

        Ok(value.and_then(|json| serde_json::from_str(&json).ok()))
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> AppResult<()> {
        let value_json = serde_json::to_string(value)?;

        self.connection().execute(
            "INSERT INTO settings (key, value_json) VALUES (?1, ?2) \
             ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json",
            params![key, value_json],
        )?;

        Ok(())
    }
}

fn migrate(connection: &Connection) -> AppResult<()> {
    let stored: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let version = usize::try_from(stored).unwrap_or(usize::MAX);

    if version > MIGRATIONS.len() {
        return Err(AppError::LocalState(format!(
            "the state database was created by a newer Qwerio (schema {version})"
        )));
    }

    for (index, migration) in MIGRATIONS.iter().enumerate().skip(version) {
        let target = index + 1;
        connection.execute_batch(&format!(
            "BEGIN; {migration} PRAGMA user_version = {target}; COMMIT;"
        ))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use qwerio_core::connection::{SqliteOptions, TlsMode};

    use super::*;

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    #[test]
    fn bindings_are_stored_per_workspace_without_secrets() {
        let state = LocalState::in_memory().expect("state");
        let workspace = WorkspaceId::new();
        let connection = ConnectionId::new();
        let binding = ConnectionBinding {
            options: ConnectionOptions::Postgres(qwerio_core::connection::PostgresOptions {
                host: "db.internal".to_owned(),
                port: 5432,
                user: "analyst".to_owned(),
                database: "sales".to_owned(),
                tls: TlsMode::VerifyFull { ca_file: None },
                connect_timeout: None,
                application_name: None,
            }),
            credential: Some(CredentialRef::for_connection(workspace, connection)),
        };

        state
            .save_binding(workspace, connection, &binding)
            .expect("save");

        assert_eq!(
            state.binding(workspace, connection).expect("load"),
            Some(binding)
        );
        assert_eq!(
            state
                .binding(WorkspaceId::new(), connection)
                .expect("other"),
            None
        );
        assert_eq!(
            state.bound_connections(workspace).expect("list"),
            vec![connection]
        );
    }

    #[test]
    fn history_respects_retention_and_can_be_disabled() {
        let state = LocalState::in_memory().expect("state");
        let workspace = WorkspaceId::new();
        let entry = |seconds_ago: u64, sql: &str| HistoryEntry {
            connection: ConnectionId::new(),
            database: None,
            sql: sql.to_owned(),
            started_at: now() - Duration::from_secs(seconds_ago),
            duration: Duration::from_millis(5),
            outcome: QueryOutcome::Succeeded,
            rows: Some(1),
        };

        state
            .set_history_retention(
                HistoryRetention {
                    enabled: true,
                    max_entries: 2,
                    max_age_days: 1,
                },
                now(),
            )
            .expect("retention");
        state
            .record_history(workspace, &entry(3 * SECONDS_PER_DAY, "old"), now())
            .expect("old");
        state
            .record_history(workspace, &entry(30, "a"), now())
            .expect("a");
        state
            .record_history(workspace, &entry(20, "b"), now())
            .expect("b");
        state
            .record_history(workspace, &entry(10, "c"), now())
            .expect("c");

        let kept: Vec<String> = state
            .history(workspace, 10)
            .expect("history")
            .into_iter()
            .map(|entry| entry.sql)
            .collect();
        assert_eq!(kept, vec!["c", "b"]);

        state
            .set_history_retention(
                HistoryRetention {
                    enabled: false,
                    ..HistoryRetention::default()
                },
                now(),
            )
            .expect("disable");
        state
            .record_history(workspace, &entry(1, "d"), now())
            .expect("ignored");
        assert!(state.history(workspace, 10).expect("history").is_empty());
    }

    #[test]
    fn drafts_round_trip_with_their_base_version() {
        let state = LocalState::in_memory().expect("state");
        let workspace = WorkspaceId::new();
        let draft = Draft {
            document: DocumentId::new(),
            text: "SELECT 'unsaved'".to_owned(),
            base: Some(Fingerprint::of(b"SELECT 1")),
        };

        state.save_draft(workspace, &draft, now()).expect("save");
        assert_eq!(
            state.drafts(workspace).expect("drafts"),
            vec![draft.clone()]
        );

        state
            .discard_draft(workspace, draft.document)
            .expect("discard");
        assert!(state.drafts(workspace).expect("drafts").is_empty());
    }

    #[test]
    fn reopening_keeps_state_and_newer_schemas_are_refused() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("state.sqlite");
        let workspace = WorkspaceId::new();
        let connection = ConnectionId::new();
        let binding = ConnectionBinding {
            options: ConnectionOptions::Sqlite(SqliteOptions::new("/data/app.sqlite")),
            credential: None,
        };

        LocalState::open(&path)
            .expect("first open")
            .save_binding(workspace, connection, &binding)
            .expect("save");
        let reopened = LocalState::open(&path).expect("reopen");
        assert_eq!(
            reopened.binding(workspace, connection).expect("load"),
            Some(binding)
        );
        drop(reopened);

        Connection::open(&path)
            .expect("raw open")
            .execute_batch("PRAGMA user_version = 99")
            .expect("bump version");
        assert!(LocalState::open(&path).is_err());
    }

    #[test]
    fn incompatible_ui_state_starts_fresh() {
        let state = LocalState::in_memory().expect("state");
        let workspace = WorkspaceId::new();
        state
            .save_ui_state(workspace, &"not the expected shape")
            .expect("save");

        let loaded: Option<Vec<u32>> = state.ui_state(workspace).expect("load");
        assert_eq!(loaded, None);
    }
}
