//! Runs one statement of user SQL on the worker and streams its events.

use std::sync::Arc;

use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::ids::ResultId;
use qwerio_core::result::{
    BudgetDecision, BudgetTracker, ColumnDescriptor, EventSender, RequestEventKind, ResultBudget,
    Row, StatementSummary,
};
use qwerio_core::session::TransactionState;
use qwerio_core::sql_text;
use rusqlite::Connection;

use crate::convert::value_from_ref;
use crate::error::map_error;

/// Statements that report a row count even when it is zero.
const ROW_COUNT_KEYWORDS: [&str; 5] = ["INSERT", "UPDATE", "DELETE", "REPLACE", "WITH"];

pub(crate) fn transaction_state(connection: &Connection) -> TransactionState {
    if connection.is_autocommit() {
        TransactionState::Idle
    } else {
        TransactionState::Active
    }
}

/// Executes `sql` and sends its events except the final `Finished`, which
/// the session sends once it accepts new requests again.
pub(crate) fn execute_statement(
    connection: &Connection,
    sql: &str,
    budget: ResultBudget,
    events: &mut EventSender,
) {
    match run_statement(connection, sql, budget, events) {
        Ok(()) => {}
        Err(error) if error.kind == ErrorKind::Cancelled => {
            events.send_blocking(RequestEventKind::Cancelled);
        }
        Err(error) => {
            events.send_blocking(RequestEventKind::Failed(error));
        }
    }
}

fn run_statement(
    connection: &Connection,
    sql: &str,
    budget: ResultBudget,
    events: &mut EventSender,
) -> DbResult<()> {
    if sql_text::is_blank(sql) {
        return Err(DatabaseError::new(
            ErrorKind::EmptyStatement,
            "There is no statement to run",
        ));
    }

    // Preparing validates the whole text: a second statement is rejected
    // before anything executes.
    let mut statement = connection.prepare(sql).map_err(map_error)?;

    if statement.parameter_count() > 0 {
        return Err(DatabaseError::new(
            ErrorKind::Parameters,
            "Query parameters are not supported here; replace placeholders with values",
        ));
    }

    if statement.column_count() == 0 {
        let changes_before = connection.total_changes();
        statement.raw_execute().map_err(map_error)?;
        let rows_affected = rows_affected(connection, sql, changes_before);

        events.send_blocking(RequestEventKind::StatementCompleted {
            result_id: None,
            summary: StatementSummary {
                rows_returned: 0,
                rows_affected,
                truncation: None,
            },
        });

        return Ok(());
    }

    let columns: Arc<[ColumnDescriptor]> = statement
        .columns()
        .iter()
        .map(|column| ColumnDescriptor::new(column.name(), column.decl_type().unwrap_or_default()))
        .collect();
    let column_count = columns.len();
    let result_id = ResultId::new();

    if !events.send_blocking(RequestEventKind::ResultStarted { result_id, columns }) {
        return Ok(());
    }

    let mut rows = statement.query([]).map_err(map_error)?;
    let mut tracker = BudgetTracker::new(budget);

    loop {
        let Some(row) = rows.next().map_err(map_error)? else {
            let batch = tracker.take_batch();

            if !batch.is_empty() && !send_rows(events, result_id, batch) {
                return Ok(());
            }

            break;
        };

        let values: Row = (0..column_count)
            .map(|index| row.get_ref(index).map(value_from_ref))
            .collect::<Result<_, _>>()
            .map_err(map_error)?;

        match tracker.offer(values) {
            BudgetDecision::Continue => {}
            BudgetDecision::Deliver(batch) => {
                if !send_rows(events, result_id, batch) {
                    return Ok(());
                }
            }
            BudgetDecision::Stop(batch) => {
                if !batch.is_empty() && !send_rows(events, result_id, batch) {
                    return Ok(());
                }

                break;
            }
        }
    }

    events.send_blocking(RequestEventKind::StatementCompleted {
        result_id: Some(result_id),
        summary: StatementSummary {
            rows_returned: tracker.rows_retained() as u64,
            rows_affected: None,
            truncation: tracker.truncation(),
        },
    });

    Ok(())
}

fn send_rows(events: &mut EventSender, result_id: ResultId, rows: Vec<Row>) -> bool {
    events.send_blocking(RequestEventKind::Rows { result_id, rows })
}

/// `changes()` keeps the previous statement's count after DDL, so it is only
/// trusted when the total change counter moved.
fn rows_affected(connection: &Connection, sql: &str, changes_before: u64) -> Option<u64> {
    if connection.total_changes() != changes_before {
        return Some(connection.changes());
    }

    let keyword = sql_text::leading_keyword(sql)?;
    ROW_COUNT_KEYWORDS.contains(&keyword.as_str()).then_some(0)
}
