//! Session metadata: purpose, access mode, enforcement, and transaction state.

use serde::{Deserialize, Serialize};

use crate::connection::EngineKind;
use crate::ids::{ConnectionId, SessionId};

/// The access the user asked for. This is UI intent; see [`ReadOnlyEnforcement`]
/// for what actually prevents writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccessMode {
    ReadOnly,
    ReadWrite,
}

/// How a read-only request is enforced by the engine, if at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReadOnlyEnforcement {
    /// The file was opened with the driver's read-only flags. Writes fail.
    ReadOnlyFile,
    /// Transactions default to read-only. Generated queries respect it, but
    /// arbitrary SQL can change the setting: this is not a security boundary.
    /// Use a restricted database role when read-only access must be guaranteed.
    TransactionDefault,
    /// The session is writable.
    NotEnforced,
}

impl ReadOnlyEnforcement {
    /// Whether arbitrary SQL on this session is guaranteed not to write.
    pub fn guarantees_no_writes(self) -> bool {
        matches!(self, Self::ReadOnlyFile)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionPurpose {
    /// Short-lived catalog and table browsing work with generated SQL.
    Browse,
    /// A retained physical connection for one SQL console.
    Console,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransactionState {
    /// Autocommit: no explicit transaction is open.
    Idle,
    /// An explicit transaction is open.
    Active,
    /// The open transaction failed; only ROLLBACK can proceed.
    Failed,
    /// The state could not be determined, for example after a lost connection.
    Unknown,
}

impl TransactionState {
    pub fn is_open(self) -> bool {
        matches!(self, Self::Active | Self::Failed)
    }
}

/// Metadata describing one open session, shown wherever SQL can execute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub connection: ConnectionId,
    pub engine: EngineKind,
    /// The database this session is connected to, for engines with several.
    pub database: Option<String>,
    pub purpose: SessionPurpose,
    pub requested_access: AccessMode,
    pub enforcement: ReadOnlyEnforcement,
    pub server_version: Option<String>,
    /// Whether the engine enforces foreign keys on this connection, when that
    /// is a per-connection setting (SQLite).
    pub foreign_keys_enforced: Option<bool>,
}
