//! SQLite sessions: one connection on one worker thread.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use qwerio_core::catalog::{ForeignKeyInfo, ObjectDetails};
use qwerio_core::driver::{
    CancelOutcome, DatabaseCatalog, DatabaseSession, QueryRequest, TableBrowser, TableEditor,
};
use qwerio_core::edit::{
    RowEditRequest, SavedRow, render_change, returned_column_count, saved_row,
};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::ids::RequestId;
use qwerio_core::paging::{Page, PagePlan, PageRequest};
use qwerio_core::resource::{CatalogEntry, CatalogPath, ObjectRef};
use qwerio_core::result::{ColumnDescriptor, EventSender, RequestEventKind, ResultStream, Row};
use qwerio_core::session::{SessionInfo, TransactionState};
use qwerio_core::sql::RenderedSql;
use rusqlite::{Connection, params_from_iter};

use crate::catalog;
use crate::convert::{SqlValue, value_from_ref};
use crate::dialect::SqliteDialect;
use crate::error::map_error;
use crate::execute::{execute_statement, transaction_state};
use crate::worker::{JobStart, Worker};

pub(crate) struct SqliteSession {
    info: SessionInfo,
    worker: Worker,
    shared: Arc<SharedState>,
}

/// State updated by worker jobs and read synchronously by the session.
struct SharedState {
    active_request: Mutex<Option<RequestId>>,
    transaction: AtomicU8,
}

impl SharedState {
    fn set_transaction(&self, state: TransactionState) {
        self.transaction
            .store(encode_state(state), Ordering::SeqCst);
    }

    fn finish_request(&self, request: RequestId) {
        let mut active = self
            .active_request
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        if *active == Some(request) {
            *active = None;
        }
    }

    fn is_busy(&self) -> bool {
        self.active_request
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }
}

fn encode_state(state: TransactionState) -> u8 {
    match state {
        TransactionState::Idle => 0,
        TransactionState::Active => 1,
        TransactionState::Failed => 2,
        TransactionState::Unknown => 3,
    }
}

fn decode_state(value: u8) -> TransactionState {
    match value {
        0 => TransactionState::Idle,
        1 => TransactionState::Active,
        2 => TransactionState::Failed,
        _ => TransactionState::Unknown,
    }
}

impl SqliteSession {
    pub(crate) fn new(info: SessionInfo, worker: Worker) -> Self {
        Self {
            info,
            worker,
            shared: Arc::new(SharedState {
                active_request: Mutex::new(None),
                transaction: AtomicU8::new(encode_state(TransactionState::Idle)),
            }),
        }
    }

    async fn run_transaction_command(&self, sql: &'static str) -> DbResult<TransactionState> {
        if self.shared.is_busy() {
            return Err(DatabaseError::session_busy());
        }

        let shared = Arc::clone(&self.shared);

        self.worker
            .call(move |connection| {
                let result = connection.execute_batch(sql).map_err(map_error);
                let state = transaction_state(connection);
                shared.set_transaction(state);
                result.map(|()| state)
            })
            .await
    }
}

#[async_trait]
impl DatabaseCatalog for SqliteSession {
    async fn list(&self, path: &CatalogPath) -> DbResult<Vec<CatalogEntry>> {
        let path = path.clone();
        self.worker
            .call(move |connection| catalog::list(connection, &path))
            .await
    }

    async fn describe(&self, object: &ObjectRef) -> DbResult<ObjectDetails> {
        let object = object.clone();
        self.worker
            .call(move |connection| catalog::describe(connection, &object))
            .await
    }

    async fn relationships(&self, path: &CatalogPath) -> DbResult<Vec<ForeignKeyInfo>> {
        let path = path.clone();
        let connection_id = self.info.connection;
        self.worker
            .call(move |connection| catalog::relationships(connection, connection_id, &path))
            .await
    }
}

#[async_trait]
impl TableBrowser for SqliteSession {
    async fn fetch_page(&self, request: PageRequest) -> DbResult<Page> {
        let plan = PagePlan::new(&request)?;
        self.worker
            .call(move |connection| fetch_page(connection, &plan))
            .await
    }
}

fn fetch_page(connection: &Connection, plan: &PagePlan) -> DbResult<Page> {
    let query = plan.render(&SqliteDialect)?;
    let mut statement = connection.prepare(&query.sql.text).map_err(map_error)?;

    let columns: Vec<ColumnDescriptor> = statement
        .columns()
        .iter()
        .map(|column| ColumnDescriptor::new(column.name(), column.decl_type().unwrap_or_default()))
        .collect();
    let column_count = columns.len();

    let raw_rows = query_rows(&mut statement, &query.sql, column_count)?;

    Ok(plan.build_page(columns, raw_rows))
}

fn query_rows(
    statement: &mut rusqlite::Statement<'_>,
    sql: &RenderedSql,
    column_count: usize,
) -> DbResult<Vec<Row>> {
    let parameters = sql.params.iter().map(SqlValue);
    let mut rows = statement
        .query(params_from_iter(parameters))
        .map_err(map_error)?;
    let mut raw_rows: Vec<Row> = Vec::new();

    while let Some(row) = rows.next().map_err(map_error)? {
        let values = (0..column_count)
            .map(|index| row.get_ref(index).map(value_from_ref))
            .collect::<Result<Row, _>>()
            .map_err(map_error)?;
        raw_rows.push(values);
    }

    Ok(raw_rows)
}

#[async_trait]
impl TableEditor for SqliteSession {
    async fn apply_row_edits(&self, request: RowEditRequest) -> DbResult<Vec<SavedRow>> {
        if self.shared.is_busy() {
            return Err(DatabaseError::session_busy());
        }

        if self.transaction_state().is_open() {
            return Err(DatabaseError::invalid_request(
                "Finish the open transaction before saving edits",
            ));
        }

        self.worker
            .call(move |connection| apply_row_edits(connection, &request))
            .await
    }
}

/// Runs every change in one transaction, rolling back on the first failure.
fn apply_row_edits(connection: &Connection, request: &RowEditRequest) -> DbResult<Vec<SavedRow>> {
    let statements = request
        .changes
        .iter()
        .map(|change| render_change(&SqliteDialect, &request.details, change))
        .collect::<DbResult<Vec<_>>>()?;

    connection.execute_batch("BEGIN").map_err(map_error)?;

    let updated = statements
        .iter()
        .map(|sql| change_one_row(connection, sql, &request.details))
        .collect::<DbResult<Vec<_>>>();

    let finish = if updated.is_ok() {
        "COMMIT"
    } else {
        "ROLLBACK"
    };
    let finished = connection.execute_batch(finish).map_err(map_error);
    let rows = updated?;

    // A failed COMMIT (deferred constraint, busy database) leaves the
    // transaction open; the commit error is the one to report.
    if let Err(error) = finished {
        if !connection.is_autocommit() {
            connection.execute_batch("ROLLBACK").map_err(map_error)?;
        }

        return Err(error);
    }

    Ok(rows)
}

fn change_one_row(
    connection: &Connection,
    sql: &RenderedSql,
    details: &ObjectDetails,
) -> DbResult<SavedRow> {
    let mut statement = connection.prepare(&sql.text).map_err(map_error)?;
    let rows = query_rows(&mut statement, sql, returned_column_count(details))?;
    saved_row(details, rows)
}

#[async_trait]
impl DatabaseSession for SqliteSession {
    fn info(&self) -> &SessionInfo {
        &self.info
    }

    fn execute(&self, request: QueryRequest) -> DbResult<ResultStream> {
        {
            let mut active = self
                .shared
                .active_request
                .lock()
                .unwrap_or_else(PoisonError::into_inner);

            if active.is_some() {
                return Err(DatabaseError::session_busy());
            }

            *active = Some(request.id);
        }

        let (mut events, stream) = EventSender::channel(request.id);
        let shared = Arc::clone(&self.shared);
        let request_id = request.id;

        let submitted = self
            .worker
            .submit(Some(request_id), move |connection, start| {
                match start {
                    JobStart::Run => {
                        execute_statement(connection, &request.sql, request.budget, &mut events)
                    }
                    JobStart::Discarded => {
                        events.send_blocking(RequestEventKind::Cancelled);
                    }
                }

                let transaction = transaction_state(connection);
                shared.set_transaction(transaction);
                shared.finish_request(request_id);
                events.send_blocking(RequestEventKind::Finished { transaction });
            });

        if let Err(error) = submitted {
            self.shared.finish_request(request_id);
            return Err(error);
        }

        Ok(stream)
    }

    async fn cancel(&self, request_id: RequestId) -> DbResult<CancelOutcome> {
        Ok(self.worker.cancel(request_id))
    }

    fn transaction_state(&self) -> TransactionState {
        decode_state(self.shared.transaction.load(Ordering::SeqCst))
    }

    async fn begin(&self) -> DbResult<TransactionState> {
        if self.transaction_state().is_open() {
            return Err(DatabaseError::new(
                ErrorKind::InvalidRequest,
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
        let result = self
            .worker
            .call(|connection| {
                if connection.is_autocommit() {
                    return Ok(());
                }

                connection.execute_batch("ROLLBACK").map_err(map_error)
            })
            .await;

        self.worker.shut_down();

        match result {
            Err(error) if error.kind == ErrorKind::SessionClosed => Ok(()),
            other => other,
        }
    }
}
