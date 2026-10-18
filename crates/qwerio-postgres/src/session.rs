//! PostgreSQL sessions: one physical connection driven on the Tokio runtime.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};

use async_trait::async_trait;
use qwerio_core::catalog::ObjectDetails;
use qwerio_core::driver::{
    CancelOutcome, DatabaseCatalog, DatabaseSession, QueryRequest, TableBrowser,
};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::ids::RequestId;
use qwerio_core::paging::{Page, PagePlan, PageRequest};
use qwerio_core::resource::{CatalogEntry, CatalogPath, ObjectRef};
use qwerio_core::result::{ColumnDescriptor, EventSender, RequestEventKind, ResultStream};
use qwerio_core::session::{SessionInfo, TransactionState};
use qwerio_core::value::{UnsupportedValue, Value};
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio_postgres::types::{ToSql, Type};
use tokio_postgres::{CancelToken, Client, NoTls};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::catalog;
use crate::decode::{decode_row, value_to_text};
use crate::dialect::PostgresDialect;
use crate::error::map_error;
use crate::execute::{Execution, execute_statement, needs_probe, probe_transaction_state};

pub(crate) struct PostgresSession {
    info: SessionInfo,
    runtime: Handle,
    /// `None` once closed. Requests hold the lock while they use the
    /// connection, which also serializes catalog and browse work.
    client: Arc<Mutex<Option<Client>>>,
    cancel_token: CancelToken,
    tls: Option<MakeRustlsConnect>,
    state: Arc<SharedState>,
}

pub(crate) struct SessionParts {
    pub info: SessionInfo,
    pub runtime: Handle,
    pub client: Client,
    pub cancel_token: CancelToken,
    pub tls: Option<MakeRustlsConnect>,
}

struct SharedState {
    active_request: StdMutex<Option<RequestId>>,
    /// Checked between protocol steps, because a server cancel request that
    /// arrives while no statement runs is ignored by PostgreSQL.
    cancel_requested: AtomicBool,
    transaction: AtomicU8,
}

impl SharedState {
    fn active_request(&self) -> Option<RequestId> {
        *self
            .active_request
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn try_start(&self, request: RequestId) -> DbResult<()> {
        let mut active = self
            .active_request
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        if active.is_some() {
            return Err(DatabaseError::session_busy());
        }

        *active = Some(request);
        self.cancel_requested.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn finish(&self, request: RequestId) {
        let mut active = self
            .active_request
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        if *active == Some(request) {
            *active = None;
        }
    }

    fn transaction(&self) -> TransactionState {
        match self.transaction.load(Ordering::SeqCst) {
            0 => TransactionState::Idle,
            1 => TransactionState::Active,
            2 => TransactionState::Failed,
            _ => TransactionState::Unknown,
        }
    }

    fn set_transaction(&self, state: TransactionState) {
        let encoded = match state {
            TransactionState::Idle => 0,
            TransactionState::Active => 1,
            TransactionState::Failed => 2,
            TransactionState::Unknown => 3,
        };
        self.transaction.store(encoded, Ordering::SeqCst);
    }
}

impl PostgresSession {
    pub(crate) fn new(parts: SessionParts) -> Self {
        Self {
            info: parts.info,
            runtime: parts.runtime,
            client: Arc::new(Mutex::new(Some(parts.client))),
            cancel_token: parts.cancel_token,
            tls: parts.tls,
            state: Arc::new(SharedState {
                active_request: StdMutex::new(None),
                cancel_requested: AtomicBool::new(false),
                transaction: AtomicU8::new(0),
            }),
        }
    }

    /// Runs async work on the adapter's runtime with the client locked.
    async fn with_client<T, F>(&self, work: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut Client) -> futures_util::future::BoxFuture<'c, DbResult<T>>
            + Send
            + 'static,
    {
        let client = Arc::clone(&self.client);

        self.runtime
            .spawn(async move {
                let mut guard = client.lock().await;
                let client = guard.as_mut().ok_or_else(DatabaseError::session_closed)?;
                work(client).await
            })
            .await
            .map_err(|error| DatabaseError::internal(format!("A database task failed: {error}")))?
    }

    async fn run_transaction_command(&self, sql: &'static str) -> DbResult<TransactionState> {
        if self.state.active_request().is_some() {
            return Err(DatabaseError::session_busy());
        }

        let state = Arc::clone(&self.state);

        self.with_client(move |client| {
            Box::pin(async move {
                let result = client.batch_execute(sql).await;
                let transaction = probe_transaction_state(client).await;
                state.set_transaction(transaction);
                result.map_err(|error| map_error(error, Some(sql)))?;
                Ok(transaction)
            })
        })
        .await
    }

    fn database(&self) -> &str {
        self.info.database.as_deref().unwrap_or_default()
    }
}

#[async_trait]
impl DatabaseCatalog for PostgresSession {
    async fn list(&self, path: &CatalogPath) -> DbResult<Vec<CatalogEntry>> {
        let path = path.clone();
        let database = self.database().to_owned();

        self.with_client(move |client| {
            Box::pin(async move { catalog::list(client, &database, &path).await })
        })
        .await
    }

    async fn describe(&self, object: &ObjectRef) -> DbResult<ObjectDetails> {
        if let Some(database) = &object.database
            && database != self.database()
        {
            return Err(DatabaseError::invalid_request(format!(
                "This session is connected to \"{}\", not \"{database}\"",
                self.database()
            )));
        }

        let object = object.clone();
        self.with_client(move |client| {
            Box::pin(async move { catalog::describe(client, &object).await })
        })
        .await
    }
}

#[async_trait]
impl TableBrowser for PostgresSession {
    async fn fetch_page(&self, request: PageRequest) -> DbResult<Page> {
        let plan = PagePlan::new(&request)?;
        let details = Arc::clone(&request.details);

        self.with_client(move |client| {
            Box::pin(async move { fetch_page(client, &plan, &details).await })
        })
        .await
    }
}

async fn fetch_page(client: &Client, plan: &PagePlan, details: &ObjectDetails) -> DbResult<Page> {
    let query = plan.render(&PostgresDialect)?;
    let sql = query.sql.text.as_str();
    let map = |error| map_error(error, Some(sql));

    let parameter_types = vec![Type::TEXT; query.sql.params.len()];
    let statement = client
        .prepare_typed(sql, &parameter_types)
        .await
        .map_err(map)?;

    let parameters = query
        .sql
        .params
        .iter()
        .map(value_to_text)
        .collect::<Result<Vec<Option<String>>, String>>()
        .map_err(|message| DatabaseError::new(ErrorKind::Parameters, message))?;
    let parameter_refs: Vec<&(dyn ToSql + Sync)> = parameters
        .iter()
        .map(|parameter| parameter as &(dyn ToSql + Sync))
        .collect();

    let rows = client
        .query(&statement, &parameter_refs)
        .await
        .map_err(map)?;

    let columns: Vec<ColumnDescriptor> = statement
        .columns()
        .iter()
        .map(|column| ColumnDescriptor::new(column.name(), column.type_().name()))
        .collect();

    let raw_rows = rows
        .iter()
        .map(|row| {
            let mut values = decode_row(row);
            mark_text_fallbacks(&mut values, details);
            values
        })
        .collect();

    let mut page = plan.build_page(columns, raw_rows);

    // Report the catalog's type names for data columns.
    for (descriptor, column) in page.columns.iter_mut().zip(&details.columns) {
        descriptor.type_name = column.type_name.clone();
        descriptor.nullable = Some(column.nullable);
    }

    Ok(page)
}

/// Columns read as text because they cannot be decoded natively become
/// unsupported values that carry the engine's text.
fn mark_text_fallbacks(values: &mut [Value], details: &ObjectDetails) {
    for (value, column) in values.iter_mut().zip(&details.columns) {
        if column.decodable {
            continue;
        }

        if let Value::Text(text) = value {
            *value = Value::Unsupported(UnsupportedValue {
                type_name: column.type_name.clone(),
                text: Some(std::mem::take(text)),
                raw: None,
            });
        }
    }
}

#[async_trait]
impl DatabaseSession for PostgresSession {
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
            let prior = state.transaction();

            let Some(client) = guard.as_mut() else {
                state.finish(request.id);
                events
                    .send(RequestEventKind::Failed(DatabaseError::session_closed()))
                    .await;
                events
                    .send(RequestEventKind::Finished {
                        transaction: TransactionState::Unknown,
                    })
                    .await;
                return;
            };

            let failed = execute_statement(Execution {
                client,
                sql: &request.sql,
                budget: request.budget,
                events: &mut events,
                transaction: prior,
                cancel_requested: &state.cancel_requested,
            })
            .await;

            let transaction = if client.is_closed() {
                TransactionState::Unknown
            } else if needs_probe(&request.sql, prior, failed) {
                probe_transaction_state(client).await
            } else {
                prior
            };

            state.set_transaction(transaction);
            drop(guard);
            state.finish(request.id);
            events
                .send(RequestEventKind::Finished { transaction })
                .await;
        });

        Ok(stream)
    }

    async fn cancel(&self, request_id: RequestId) -> DbResult<CancelOutcome> {
        if self.state.active_request() != Some(request_id) {
            return Ok(CancelOutcome::NotRunning);
        }

        self.state.cancel_requested.store(true, Ordering::SeqCst);

        let token = self.cancel_token.clone();
        let tls = self.tls.clone();

        self.runtime
            .spawn(async move {
                match tls {
                    Some(tls) => token.cancel_query(tls).await,
                    None => token.cancel_query(NoTls).await,
                }
            })
            .await
            .map_err(|error| DatabaseError::internal(format!("The cancel task failed: {error}")))?
            .map_err(|error| map_error(error, None))?;

        Ok(CancelOutcome::Requested)
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

        self.run_transaction_command("BEGIN").await
    }

    async fn commit(&self) -> DbResult<TransactionState> {
        self.run_transaction_command("COMMIT").await
    }

    async fn rollback(&self) -> DbResult<TransactionState> {
        self.run_transaction_command("ROLLBACK").await
    }

    async fn close(&self) -> DbResult<()> {
        let client = Arc::clone(&self.client);
        let state = Arc::clone(&self.state);

        self.runtime
            .spawn(async move {
                let mut guard = client.lock().await;

                let Some(client) = guard.take() else {
                    return Ok(());
                };

                let result = if state.transaction().is_open() && !client.is_closed() {
                    client
                        .batch_execute("ROLLBACK")
                        .await
                        .map_err(|error| map_error(error, None))
                } else {
                    Ok(())
                };

                state.set_transaction(TransactionState::Idle);
                // Dropping the client ends the connection task.
                drop(client);
                result
            })
            .await
            .map_err(|error| DatabaseError::internal(format!("Closing failed: {error}")))?
    }
}
