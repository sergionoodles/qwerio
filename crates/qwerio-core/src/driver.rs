//! Driver, session, catalog, and browser contracts implemented by adapters.

use std::sync::Arc;

use async_trait::async_trait;

use crate::capabilities::DatabaseCapabilities;
use crate::catalog::ObjectDetails;
use crate::connection::{ConnectionDescriptor, ConnectionOptions, Credentials, EngineKind};
use crate::error::DbResult;
use crate::ids::RequestId;
use crate::paging::{Page, PageRequest};
use crate::resource::{CatalogEntry, CatalogPath, ObjectRef};
use crate::result::{ResultBudget, ResultStream};
use crate::session::{AccessMode, SessionInfo, SessionPurpose, TransactionState};

#[derive(Clone, Debug)]
pub struct OpenSessionRequest {
    pub connection: ConnectionDescriptor,
    pub credentials: Credentials,
    pub purpose: SessionPurpose,
    pub access: AccessMode,
    /// For engines with several databases: open this one instead of the
    /// connection's default database.
    pub database: Option<String>,
}

/// One statement of user SQL. Exactly one statement is accepted; adapters
/// reject extra statements before executing anything.
#[derive(Clone, Debug)]
pub struct QueryRequest {
    pub id: RequestId,
    pub sql: String,
    pub budget: ResultBudget,
}

impl QueryRequest {
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            id: RequestId::new(),
            sql: sql.into(),
            budget: ResultBudget::DEFAULT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    /// Cancellation was requested. The request still reports how it ended.
    Requested,
    /// A queued request was discarded before it started.
    Discarded,
    /// The request is not running on this session.
    NotRunning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationIssue {
    pub field: &'static str,
    pub message: String,
}

#[async_trait]
pub trait DatabaseDriver: Send + Sync {
    fn engine(&self) -> EngineKind;

    fn capabilities(&self) -> &DatabaseCapabilities;

    /// Checks options without connecting. Empty when the options are usable.
    fn validate(&self, options: &ConnectionOptions) -> Vec<ValidationIssue>;

    async fn open(&self, request: OpenSessionRequest) -> DbResult<Arc<dyn DatabaseSession>>;
}

/// Catalog discovery. The hierarchy is the adapter's: listing the root of a
/// PostgreSQL connection returns databases, while SQLite returns objects.
#[async_trait]
pub trait DatabaseCatalog: Send + Sync {
    async fn list(&self, path: &CatalogPath) -> DbResult<Vec<CatalogEntry>>;

    async fn describe(&self, object: &ObjectRef) -> DbResult<ObjectDetails>;
}

/// Bounded, filtered, sorted pages generated from structured requests.
#[async_trait]
pub trait TableBrowser: Send + Sync {
    async fn fetch_page(&self, request: PageRequest) -> DbResult<Page>;
}

/// One open session. Console sessions retain one physical connection so that
/// BEGIN, later statements, and COMMIT/ROLLBACK share it; a session never
/// reconnects silently.
#[async_trait]
pub trait DatabaseSession: DatabaseCatalog + TableBrowser {
    fn info(&self) -> &SessionInfo;

    /// Starts one statement and returns its ordered event stream. Fails
    /// immediately with `SessionBusy` while another request runs.
    fn execute(&self, request: QueryRequest) -> DbResult<ResultStream>;

    /// Best-effort cancellation; the request's events report the outcome.
    async fn cancel(&self, request_id: RequestId) -> DbResult<CancelOutcome>;

    fn transaction_state(&self) -> TransactionState;

    async fn begin(&self) -> DbResult<TransactionState>;

    async fn commit(&self) -> DbResult<TransactionState>;

    async fn rollback(&self) -> DbResult<TransactionState>;

    /// Rolls back any open transaction and releases the connection.
    async fn close(&self) -> DbResult<()>;
}
