//! Serialize each session's work on its retained physical connection.

use crate::dialect::MysqlDialect;
use crate::error::map_error;
use crate::{catalog, connect, decode, edits, execute};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts};
use qwerio_core::catalog::{ForeignKeyInfo, ObjectDetails};
use qwerio_core::driver::{
    CancelOutcome, DatabaseCatalog, DatabaseSession, QueryRequest, TableBrowser, TableEditor,
};
use qwerio_core::edit::{RowEditRequest, SavedRow};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::ids::RequestId;
use qwerio_core::paging::{Page, PagePlan, PageRequest};
use qwerio_core::resource::{CatalogEntry, CatalogPath, ObjectRef};
use qwerio_core::result::{EventSender, RequestEventKind, ResultStream};
use qwerio_core::session::{AccessMode, SessionInfo, TransactionState};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::Mutex;

pub(crate) struct MysqlSession {
    info: SessionInfo,
    visible_database: Option<String>,
    runtime: Handle,
    client: Arc<Mutex<Option<Conn>>>,
    control_options: Opts,
    timeout: Option<Duration>,
    connection_id: u32,
    state: Arc<SharedState>,
}

struct SharedState {
    active: StdMutex<Option<RequestId>>,
    cancelled: AtomicBool,
    transaction: AtomicU8,
    // No request may finish (and allow a new query) while KILL is in flight.
    cancellation_fence: Mutex<()>,
}

impl SharedState {
    fn active(&self) -> Option<RequestId> {
        *self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn try_start(&self, request: RequestId) -> DbResult<()> {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if active.is_some() {
            return Err(DatabaseError::session_busy());
        }
        *active = Some(request);
        self.cancelled.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn finish(&self) {
        *self.active.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn transaction(&self) -> TransactionState {
        match self.transaction.load(Ordering::SeqCst) {
            0 => TransactionState::Idle,
            1 => TransactionState::Active,
            _ => TransactionState::Unknown,
        }
    }

    fn set_transaction(&self, transaction: TransactionState) {
        self.transaction.store(
            match transaction {
                TransactionState::Idle => 0,
                TransactionState::Active => 1,
                _ => 2,
            },
            Ordering::SeqCst,
        );
    }
}

impl MysqlSession {
    pub(crate) fn new(
        info: SessionInfo,
        runtime: Handle,
        client: Conn,
        control_options: Opts,
        timeout: Option<Duration>,
        visible_database: Option<String>,
    ) -> Self {
        Self {
            info,
            visible_database,
            runtime,
            connection_id: client.id(),
            client: Arc::new(Mutex::new(Some(client))),
            control_options,
            timeout,
            state: Arc::new(SharedState {
                active: StdMutex::new(None),
                cancelled: AtomicBool::new(false),
                transaction: AtomicU8::new(0),
                cancellation_fence: Mutex::new(()),
            }),
        }
    }

    async fn with_client<T, F>(&self, work: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a mut Conn) -> BoxFuture<'a, DbResult<T>> + Send + 'static,
    {
        let client = Arc::clone(&self.client);
        self.runtime
            .spawn(async move {
                let mut guard = client.lock().await;
                work(guard.as_mut().ok_or_else(DatabaseError::session_closed)?).await
            })
            .await
            .map_err(|error| DatabaseError::internal(format!("A database task failed: {error}")))?
    }

    async fn transaction_command(&self, sql: &'static str) -> DbResult<TransactionState> {
        if self.state.active().is_some() {
            return Err(DatabaseError::session_busy());
        }
        let state = Arc::clone(&self.state);
        self.with_client(move |client| {
            Box::pin(async move {
                let outcome = client.query_drop(sql).await.map_err(map_error);
                let transaction = execute::transaction_state(client).await;
                state.set_transaction(transaction);
                outcome?;
                Ok(transaction)
            })
        })
        .await
    }
}

#[async_trait]
impl DatabaseCatalog for MysqlSession {
    async fn list(&self, path: &CatalogPath) -> DbResult<Vec<CatalogEntry>> {
        let path = path.clone();
        let visible_database = self.visible_database.clone();
        self.with_client(move |client| {
            Box::pin(async move { catalog::list(client, visible_database.as_deref(), &path).await })
        })
        .await
    }

    async fn describe(&self, object: &ObjectRef) -> DbResult<ObjectDetails> {
        let object = object.clone();
        self.with_client(move |client| {
            Box::pin(async move { catalog::describe(client, &object).await })
        })
        .await
    }

    async fn relationships(&self, path: &CatalogPath) -> DbResult<Vec<ForeignKeyInfo>> {
        let path = path.clone();
        let connection = self.info.connection;
        self.with_client(move |client| {
            Box::pin(async move { catalog::relationships(client, connection, &path).await })
        })
        .await
    }
}

#[async_trait]
impl TableBrowser for MysqlSession {
    async fn fetch_page(&self, request: PageRequest) -> DbResult<Page> {
        let plan = PagePlan::new(&request)?;
        self.with_client(move |client| {
            Box::pin(async move {
                let query = plan.render(&MysqlDialect)?;
                let params = decode::parameters(&query.sql.params)?;
                let mut result = client
                    .exec_iter(&query.sql.text, params)
                    .await
                    .map_err(map_error)?;
                let columns = decode::columns(result.columns_ref());
                let mut rows = Vec::new();
                while let Some(row) = result.next().await.map_err(map_error)? {
                    rows.push(decode::decode_row(row));
                }
                result.drop_result().await.map_err(map_error)?;
                let mut page = plan.build_page(columns, rows);
                for (descriptor, column) in page.columns.iter_mut().zip(&request.details.columns) {
                    descriptor.type_name = column.type_name.clone();
                    descriptor.nullable = Some(column.nullable);
                }
                Ok(page)
            })
        })
        .await
    }
}

#[async_trait]
impl TableEditor for MysqlSession {
    async fn apply_row_edits(&self, request: RowEditRequest) -> DbResult<Vec<SavedRow>> {
        if self.info.requested_access == AccessMode::ReadOnly {
            return Err(DatabaseError::new(
                ErrorKind::ReadOnly,
                "This session is read-only",
            ));
        }
        if self.state.active().is_some() {
            return Err(DatabaseError::session_busy());
        }
        let state = Arc::clone(&self.state);
        self.with_client(move |client| {
            Box::pin(async move {
                if execute::transaction_state(client).await != TransactionState::Idle {
                    return Err(DatabaseError::invalid_request(
                        "Finish the open transaction before saving edits",
                    ));
                }
                let outcome = edits::apply(client, &request).await;
                state.set_transaction(execute::transaction_state(client).await);
                outcome
            })
        })
        .await
    }
}

#[async_trait]
impl DatabaseSession for MysqlSession {
    fn info(&self) -> &SessionInfo {
        &self.info
    }

    fn execute(&self, request: QueryRequest) -> DbResult<ResultStream> {
        self.state.try_start(request.id)?;
        let (mut events, stream) = EventSender::channel(request.id);
        let client = Arc::clone(&self.client);
        let state = Arc::clone(&self.state);
        self.runtime.spawn(async move {
            let mut guard = client.lock().await;
            let outcome = match guard.as_mut() {
                Some(client) => {
                    execute::execute(client, &request, &mut events, &state.cancelled).await
                }
                None => Err(DatabaseError::session_closed()),
            };
            let _fence = state.cancellation_fence.lock().await;
            match outcome {
                Err(error) if error.kind == ErrorKind::Cancelled => {
                    events.send(RequestEventKind::Cancelled).await;
                }
                Err(error) => {
                    events.send(RequestEventKind::Failed(error)).await;
                }
                Ok(()) => {}
            }
            let transaction = match guard.as_mut() {
                Some(client) => execute::transaction_state(client).await,
                None => TransactionState::Unknown,
            };
            state.set_transaction(transaction);
            drop(guard);
            state.finish();
            events
                .send(RequestEventKind::Finished { transaction })
                .await;
        });
        Ok(stream)
    }

    async fn apply_permission_change(
        &self,
        change: &qwerio_core::permissions::PermissionChange,
    ) -> DbResult<()> {
        if self.info.requested_access != AccessMode::ReadWrite {
            return Err(DatabaseError::new(
                ErrorKind::ReadOnly,
                "Permission changes require a writable session",
            ));
        }
        if self.state.active().is_some() {
            return Err(DatabaseError::session_busy());
        }
        change.preview(self.info.engine)?;
        let change = change.clone();
        let engine = self.info.engine;
        let state = Arc::clone(&self.state);
        self.with_client(move |client| Box::pin(async move {
            if execute::transaction_state(client).await != TransactionState::Idle {
                return Err(DatabaseError::invalid_request("Finish the transaction before changing permissions"));
            }
            // Only typed, quoted administration SQL reaches the text protocol.
            // Never fall back for arbitrary SQL: it can contain extra statements.
            client.query_drop("SET SESSION sql_mode = CONCAT_WS(',', NULLIF(@@SESSION.sql_mode, ''), 'NO_BACKSLASH_ESCAPES')").await.map_err(map_error)?;
            let literal_database_names = if engine == qwerio_core::connection::EngineKind::Mysql {
                let variable: Option<(String, String)> = client.query_first("SHOW GLOBAL VARIABLES LIKE 'partial_revokes'").await.map_err(map_error)?;
                variable.is_some_and(|(_, value)| value.eq_ignore_ascii_case("ON"))
            } else {
                false
            };
            let sql = change.render_for_server(engine, literal_database_names)?;
            let outcome = client.query_drop(sql).await.map_err(map_error);
            state.set_transaction(execute::transaction_state(client).await);
            outcome
        })).await
    }

    async fn cancel(&self, request: RequestId) -> DbResult<CancelOutcome> {
        let state = Arc::clone(&self.state);
        let options = self.control_options.clone();
        let timeout = self.timeout;
        let id = self.connection_id;
        self.runtime
            .spawn(async move {
                let _fence = state.cancellation_fence.lock().await;
                if state.active() != Some(request) {
                    return Ok(CancelOutcome::NotRunning);
                }
                state.cancelled.store(true, Ordering::SeqCst);
                let mut control = connect::connect(options, timeout).await?;
                let outcome = control
                    .query_drop(format!("KILL QUERY {id}"))
                    .await
                    .map_err(map_error);
                let closed = control.disconnect().await.map_err(map_error);
                outcome.and(closed)?;
                Ok(CancelOutcome::Requested)
            })
            .await
            .map_err(|error| DatabaseError::internal(format!("The cancel task failed: {error}")))?
    }

    fn transaction_state(&self) -> TransactionState {
        self.state.transaction()
    }

    async fn begin(&self) -> DbResult<TransactionState> {
        if self.transaction_state().is_open() {
            return Err(DatabaseError::invalid_request(
                "A transaction is already open",
            ));
        }
        self.transaction_command("START TRANSACTION").await
    }

    async fn commit(&self) -> DbResult<TransactionState> {
        self.transaction_command("COMMIT").await
    }

    async fn rollback(&self) -> DbResult<TransactionState> {
        self.transaction_command("ROLLBACK").await
    }

    async fn close(&self) -> DbResult<()> {
        if let Some(request) = self.state.active() {
            self.cancel(request).await?;
        }
        let client = Arc::clone(&self.client);
        let state = Arc::clone(&self.state);
        self.runtime
            .spawn(async move {
                let mut guard = client.lock().await;
                if let Some(mut client) = guard.take() {
                    // Roll back explicitly; dropping a TCP connection also rolls back.
                    let rolled_back = client.query_drop("ROLLBACK").await.map_err(map_error);
                    let closed = client.disconnect().await.map_err(map_error);
                    state.set_transaction(TransactionState::Unknown);
                    rolled_back.and(closed)?;
                }
                Ok(())
            })
            .await
            .map_err(|error| DatabaseError::internal(format!("The close task failed: {error}")))?
    }
}
