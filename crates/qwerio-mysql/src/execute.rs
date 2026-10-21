//! Prepared protocol rejects extra statements before executing any SQL.

use crate::error::map_error;
use crate::{decode, sql_text};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Params};
use qwerio_core::driver::QueryRequest;
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::ids::ResultId;
use qwerio_core::result::{
    BudgetDecision, BudgetTracker, EventSender, RequestEventKind, StatementSummary,
};
use qwerio_core::session::TransactionState;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) async fn transaction_state(connection: &mut Conn) -> TransactionState {
    // COM_PING obtains fresh status even after an error cleared the last OK
    // packet. It neither starts nor commits a transaction.
    if connection.ping().await.is_err() {
        return TransactionState::Unknown;
    }
    match connection.last_ok_packet() {
        Some(packet)
            if packet
                .status_flags()
                .contains(mysql_async::consts::StatusFlags::SERVER_STATUS_IN_TRANS) =>
        {
            TransactionState::Active
        }
        Some(_) => TransactionState::Idle,
        None => TransactionState::Unknown,
    }
}

pub(crate) async fn execute(
    connection: &mut Conn,
    request: &QueryRequest,
    events: &mut EventSender,
    cancelled: &AtomicBool,
) -> DbResult<()> {
    if sql_text::statement_count(&request.sql) == 0 {
        return Err(DatabaseError::new(
            ErrorKind::EmptyStatement,
            "There is no statement to run",
        ));
    }
    let prepared = connection.prep(&request.sql).await;
    if let Err(mysql_async::Error::Server(error)) = &prepared
        && error.code == 1295
        && sql_text::is_begin(&request.sql)
    {
        // MySQL excludes BEGIN from prepared SQL; execute its canonical
        // equivalent, never arbitrary user text through the multi-query API.
        if cancelled.load(Ordering::SeqCst) {
            return Err(DatabaseError::cancelled());
        }
        connection
            .query_drop("START TRANSACTION")
            .await
            .map_err(map_error)?;
        events
            .send(RequestEventKind::StatementCompleted {
                result_id: None,
                summary: StatementSummary {
                    rows_returned: 0,
                    rows_affected: None,
                    truncation: None,
                },
            })
            .await;
        return Ok(());
    }
    let statement = prepared.map_err(|error| {
        let error = map_error(error);
        if error.kind == ErrorKind::Syntax && sql_text::statement_count(&request.sql) > 1 {
            return DatabaseError::new(
                ErrorKind::MultipleStatements,
                "Run one statement at a time",
            );
        }
        if error.kind == ErrorKind::Syntax {
            return match syntax_position(&request.sql, &error.message) {
                Some(position) => error.with_position(position),
                None => error,
            };
        }
        error
    })?;
    if !statement.params().is_empty() {
        return Err(DatabaseError::new(
            ErrorKind::Parameters,
            "Query parameters are not supported here; replace placeholders with values",
        ));
    }
    if cancelled.load(Ordering::SeqCst) {
        return Err(DatabaseError::cancelled());
    }
    let mut result = connection
        .exec_iter(&statement, Params::Empty)
        .await
        .map_err(map_error)?;
    loop {
        let columns = decode::columns(result.columns_ref());
        let result_id = (!columns.is_empty()).then(ResultId::new);
        if let Some(result_id) = result_id {
            events
                .send(RequestEventKind::ResultStarted {
                    result_id,
                    columns: Arc::from(columns),
                })
                .await;
        }
        let mut tracker = BudgetTracker::new(request.budget);
        let mut stopped = false;
        while let Some(row) = result.next().await.map_err(map_error)? {
            if stopped {
                continue;
            }
            let (batch, stop) = match tracker.offer(decode::decode_row(row)) {
                BudgetDecision::Continue => continue,
                BudgetDecision::Deliver(rows) => (rows, false),
                BudgetDecision::Stop(rows) => (rows, true),
            };
            stopped = stop;
            if !batch.is_empty()
                && let Some(result_id) = result_id
            {
                stopped |= !events
                    .send(RequestEventKind::Rows {
                        result_id,
                        rows: batch,
                    })
                    .await;
            }
        }
        if cancelled.load(Ordering::SeqCst) {
            result.drop_result().await.map_err(map_error)?;
            return Err(DatabaseError::cancelled());
        }
        let batch = tracker.take_batch();
        if !batch.is_empty()
            && let Some(result_id) = result_id
        {
            events
                .send(RequestEventKind::Rows {
                    result_id,
                    rows: batch,
                })
                .await;
        }
        events
            .send(RequestEventKind::StatementCompleted {
                result_id,
                summary: StatementSummary {
                    rows_returned: tracker.rows_retained() as u64,
                    rows_affected: result_id.is_none().then(|| result.affected_rows()),
                    truncation: tracker.truncation(),
                },
            })
            .await;
        if result.is_empty() {
            break;
        }
    }
    result.drop_result().await.map_err(map_error)?;
    if cancelled.load(Ordering::SeqCst) {
        // KILL QUERY can make MySQL SLEEP return 1 rather than an error.
        return Err(DatabaseError::cancelled());
    }
    Ok(())
}

fn syntax_position(sql: &str, message: &str) -> Option<usize> {
    let fragment = message
        .rsplit_once("near '")?
        .1
        .rsplit_once("' at line ")?
        .0;
    if fragment.is_empty() {
        return Some(sql.len());
    }
    sql.rfind(fragment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_positions_use_the_server_fragment() {
        let sql = "SELECT id FROM items WHERE id = = 1";
        let message = "You have an error near '= 1' at line 1";
        assert_eq!(syntax_position(sql, message), sql.rfind("= 1"));
        assert_eq!(syntax_position(sql, "No position available"), None);
    }
}
