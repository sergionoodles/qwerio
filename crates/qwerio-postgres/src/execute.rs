//! Runs one statement of user SQL and streams its events.
//!
//! Every statement goes through the extended protocol: preparing it validates
//! that the text holds exactly one statement before anything executes.
//!
//! Row-returning statements outside an explicit transaction run in a portal
//! inside a short transaction and are fetched in batches, so the server stops
//! producing rows when the budget is reached or the consumer goes away.
//! Inside a user transaction no wrapper transaction can be opened; rows then
//! stream, and the remainder of a truncated result is drained and discarded.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::{TryStreamExt, pin_mut};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::ids::ResultId;
use qwerio_core::result::{
    BudgetDecision, BudgetTracker, ColumnDescriptor, EventSender, RequestEventKind, ResultBudget,
    Row, StatementSummary,
};
use qwerio_core::session::TransactionState;
use qwerio_core::sql_text;
use tokio_postgres::error::SqlState;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, Statement};

use crate::decode::decode_row;
use crate::error::map_error;

/// Statements whose command tag count is a meaningful row count.
const ROW_COUNT_KEYWORDS: [&str; 7] = [
    "INSERT", "UPDATE", "DELETE", "MERGE", "COPY", "SELECT", "WITH",
];

const PROBE_SAVEPOINT: &str = "SAVEPOINT qwerio_transaction_probe";
const PROBE_RELEASE: &str = "RELEASE SAVEPOINT qwerio_transaction_probe";

pub(crate) struct Execution<'a> {
    pub client: &'a mut Client,
    pub sql: &'a str,
    pub budget: ResultBudget,
    pub events: &'a mut EventSender,
    pub transaction: TransactionState,
    pub cancel_requested: &'a AtomicBool,
}

impl Execution<'_> {
    fn check_cancelled(&self) -> DbResult<()> {
        if self.cancel_requested.load(Ordering::SeqCst) {
            return Err(DatabaseError::cancelled());
        }

        Ok(())
    }

    fn map(&self, error: tokio_postgres::Error) -> DatabaseError {
        map_error(error, Some(self.sql))
    }
}

/// Runs the statement and sends its events except the final `Finished`.
/// Returns whether it failed, which decides if the transaction state must be
/// re-checked.
pub(crate) async fn execute_statement(mut execution: Execution<'_>) -> bool {
    match run_statement(&mut execution).await {
        Ok(()) => false,
        Err(error) if error.kind == ErrorKind::Cancelled => {
            execution.events.send(RequestEventKind::Cancelled).await;
            true
        }
        Err(error) => {
            execution.events.send(RequestEventKind::Failed(error)).await;
            true
        }
    }
}

async fn run_statement(execution: &mut Execution<'_>) -> DbResult<()> {
    if sql_text::is_blank(execution.sql) {
        return Err(DatabaseError::new(
            ErrorKind::EmptyStatement,
            "There is no statement to run",
        ));
    }

    let statement = execution
        .client
        .prepare(execution.sql)
        .await
        .map_err(|error| execution.map(error))?;

    if !statement.params().is_empty() {
        return Err(DatabaseError::new(
            ErrorKind::Parameters,
            "Query parameters are not supported here; replace placeholders with values",
        ));
    }

    execution.check_cancelled()?;

    if statement.columns().is_empty() {
        return run_command(execution, &statement).await;
    }

    let columns: Arc<[ColumnDescriptor]> = statement
        .columns()
        .iter()
        .map(|column| ColumnDescriptor::new(column.name(), column.type_().name()))
        .collect();
    let result_id = ResultId::new();

    if !execution
        .events
        .send(RequestEventKind::ResultStarted { result_id, columns })
        .await
    {
        return Ok(());
    }

    let tracker = if execution.transaction == TransactionState::Idle {
        fetch_through_portal(execution, &statement, result_id).await?
    } else {
        fetch_streaming(execution, &statement, result_id).await?
    };

    execution
        .events
        .send(RequestEventKind::StatementCompleted {
            result_id: Some(result_id),
            summary: StatementSummary {
                rows_returned: tracker.rows_retained() as u64,
                rows_affected: None,
                truncation: tracker.truncation(),
            },
        })
        .await;

    Ok(())
}

async fn run_command(execution: &mut Execution<'_>, statement: &Statement) -> DbResult<()> {
    let count = execution
        .client
        .execute_raw(statement, no_parameters())
        .await
        .map_err(|error| execution.map(error))?;

    let reports_count = sql_text::leading_keyword(execution.sql)
        .is_some_and(|keyword| ROW_COUNT_KEYWORDS.contains(&keyword.as_str()));

    execution
        .events
        .send(RequestEventKind::StatementCompleted {
            result_id: None,
            summary: StatementSummary {
                rows_returned: 0,
                rows_affected: reports_count.then_some(count),
                truncation: None,
            },
        })
        .await;

    Ok(())
}

fn no_parameters() -> std::iter::Empty<&'static (dyn ToSql + Sync)> {
    std::iter::empty()
}

/// What to do after offering rows to the tracker.
enum Flow {
    Continue,
    Stop,
}

async fn offer_rows(
    events: &mut EventSender,
    tracker: &mut BudgetTracker,
    result_id: ResultId,
    rows: impl IntoIterator<Item = Row>,
) -> Flow {
    for row in rows {
        match tracker.offer(row) {
            BudgetDecision::Continue => {}
            BudgetDecision::Deliver(batch) => {
                if !events
                    .send(RequestEventKind::Rows {
                        result_id,
                        rows: batch,
                    })
                    .await
                {
                    return Flow::Stop;
                }
            }
            BudgetDecision::Stop(batch) => {
                if !batch.is_empty() {
                    events
                        .send(RequestEventKind::Rows {
                            result_id,
                            rows: batch,
                        })
                        .await;
                }

                return Flow::Stop;
            }
        }
    }

    Flow::Continue
}

async fn flush(events: &mut EventSender, tracker: &mut BudgetTracker, result_id: ResultId) {
    let batch = tracker.take_batch();

    if !batch.is_empty() {
        events
            .send(RequestEventKind::Rows {
                result_id,
                rows: batch,
            })
            .await;
    }
}

async fn fetch_through_portal(
    execution: &mut Execution<'_>,
    statement: &Statement,
    result_id: ResultId,
) -> DbResult<BudgetTracker> {
    let sql = execution.sql;
    let map = |error| map_error(error, Some(sql));
    let batch_rows = execution.budget.batch_rows.max(1);
    let fetch_size = i32::try_from(batch_rows).unwrap_or(i32::MAX);
    let mut tracker = BudgetTracker::new(execution.budget);

    let transaction = execution.client.transaction().await.map_err(map)?;
    let portal = transaction.bind(statement, &[]).await.map_err(map)?;

    loop {
        if execution.cancel_requested.load(Ordering::SeqCst) {
            return Err(DatabaseError::cancelled());
        }

        let rows = transaction
            .query_portal(&portal, fetch_size)
            .await
            .map_err(map)?;
        let is_exhausted = rows.len() < batch_rows;
        let decoded = rows.iter().map(decode_row);

        if let Flow::Stop = offer_rows(execution.events, &mut tracker, result_id, decoded).await {
            break;
        }

        if is_exhausted {
            flush(execution.events, &mut tracker, result_id).await;
            break;
        }
    }

    // The statement ran as in autocommit mode; committing closes the portal.
    transaction.commit().await.map_err(map)?;
    Ok(tracker)
}

async fn fetch_streaming(
    execution: &mut Execution<'_>,
    statement: &Statement,
    result_id: ResultId,
) -> DbResult<BudgetTracker> {
    let sql = execution.sql;
    let map = |error| map_error(error, Some(sql));
    let mut tracker = BudgetTracker::new(execution.budget);

    let stream = execution
        .client
        .query_raw(statement, no_parameters())
        .await
        .map_err(map)?;
    pin_mut!(stream);

    loop {
        let Some(row) = stream.try_next().await.map_err(map)? else {
            flush(execution.events, &mut tracker, result_id).await;
            break;
        };

        if let Flow::Stop = offer_rows(
            execution.events,
            &mut tracker,
            result_id,
            [decode_row(&row)],
        )
        .await
        {
            break;
        }
    }

    Ok(tracker)
}

/// Whether the state must be re-checked after a statement.
pub(crate) fn needs_probe(sql: &str, prior: TransactionState, failed: bool) -> bool {
    let changes_state = sql_text::leading_keyword(sql)
        .is_some_and(|keyword| sql_text::may_change_transaction_state(&keyword));

    changes_state || prior == TransactionState::Unknown || (failed && prior.is_open())
}

/// Determines the server's transaction state. tokio-postgres does not expose
/// the protocol's status byte, but SAVEPOINT succeeds only inside an open
/// transaction and reports distinct errors when idle or failed.
pub(crate) async fn probe_transaction_state(client: &Client) -> TransactionState {
    match client.batch_execute(PROBE_SAVEPOINT).await {
        Ok(()) => match client.batch_execute(PROBE_RELEASE).await {
            Ok(()) => TransactionState::Active,
            Err(_) => TransactionState::Unknown,
        },
        Err(error) => match error.code() {
            Some(&SqlState::NO_ACTIVE_SQL_TRANSACTION) => TransactionState::Idle,
            Some(&SqlState::IN_FAILED_SQL_TRANSACTION) => TransactionState::Failed,
            _ => TransactionState::Unknown,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_after_transaction_statements_and_failures_inside_transactions() {
        assert!(needs_probe("commit", TransactionState::Active, false));
        assert!(needs_probe("SELECT 1", TransactionState::Active, true));
        assert!(!needs_probe("SELECT 1", TransactionState::Idle, true));
        assert!(!needs_probe("SELECT 1", TransactionState::Active, false));
    }
}
