//! What an engine adapter supports, so services and views never branch on the
//! engine itself.

use serde::{Deserialize, Serialize};

use crate::filter::FilterOperator;
use crate::session::{AccessMode, ReadOnlyEnforcement};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseCapabilities {
    /// A connection exposes several databases, each opened by its own session.
    pub has_databases: bool,
    /// Objects live in user-visible namespaces (schemas).
    pub has_namespaces: bool,
    pub access_modes: Vec<AccessMode>,
    /// How a read-only session prevents writes.
    pub read_only_enforcement: ReadOnlyEnforcement,
    /// DDL statements participate in transactions.
    pub transactional_ddl: bool,
    pub cancellation: CancellationSupport,
    pub filter_operators: Vec<FilterOperator>,
    /// Case-insensitive filters fold non-ASCII letters too.
    pub unicode_case_folding: bool,
    /// An error inside an explicit transaction aborts it until ROLLBACK.
    pub errors_abort_transactions: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancellationSupport {
    /// A separate cancel request to the server; best-effort and racy.
    ServerCancelRequest,
    /// An in-process interrupt of the running statement.
    Interrupt,
}
