//! State of one SQL console: its explicit execution target, the running
//! request, the latest result, and the session's transaction state.
//!
//! A console never runs anything on its own: restoring or opening a console
//! leaves it idle until the user runs a statement.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use qwerio_core::error::DatabaseError;
use qwerio_core::ids::{ConnectionId, RequestId, ResultId};
use qwerio_core::result::{
    ColumnDescriptor, RequestEvent, RequestEventKind, Row, StatementSummary,
};
use qwerio_core::session::{AccessMode, TransactionState};
use serde::{Deserialize, Serialize};

use crate::local_state::{HistoryEntry, QueryOutcome};

/// Where a console runs SQL. Always shown to the user.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExecutionTarget {
    pub connection: ConnectionId,
    pub database: Option<String>,
    pub access: AccessMode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResultView {
    pub columns: Arc<[ColumnDescriptor]>,
    pub rows: Vec<Row>,
    pub summary: Option<StatementSummary>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RunOutcome {
    Succeeded,
    Failed(DatabaseError),
    Cancelled,
}

/// The last finished run, for the status line and history.
#[derive(Clone, Debug, PartialEq)]
pub struct FinishedRun {
    pub sql: String,
    pub outcome: RunOutcome,
    pub duration: Duration,
    pub summary: Option<StatementSummary>,
}

struct RunningRequest {
    id: RequestId,
    sql: String,
    started: Instant,
    started_at: SystemTime,
    cancel_requested: bool,
}

pub struct ConsoleState {
    target: Option<ExecutionTarget>,
    transaction: TransactionState,
    running: Option<RunningRequest>,
    result: Option<ResultView>,
    result_id: Option<ResultId>,
    summary: Option<StatementSummary>,
    error: Option<DatabaseError>,
    was_cancelled: bool,
    last_run: Option<FinishedRun>,
}

/// What finished, for the caller to record in history.
#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    pub run: FinishedRun,
    pub history: Option<HistoryEntry>,
}

impl ConsoleState {
    pub fn new(target: Option<ExecutionTarget>) -> Self {
        Self {
            target,
            transaction: TransactionState::Idle,
            running: None,
            result: None,
            result_id: None,
            summary: None,
            error: None,
            was_cancelled: false,
            last_run: None,
        }
    }

    pub fn target(&self) -> Option<&ExecutionTarget> {
        self.target.as_ref()
    }

    /// Changing the target requires a new session; the caller closes the old
    /// one (rolling back after confirmation if a transaction is open).
    pub fn set_target(&mut self, target: Option<ExecutionTarget>) {
        self.target = target;
        self.transaction = TransactionState::Idle;
    }

    pub fn transaction(&self) -> TransactionState {
        self.transaction
    }

    pub fn set_transaction(&mut self, state: TransactionState) {
        self.transaction = state;
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn running_request(&self) -> Option<RequestId> {
        self.running.as_ref().map(|running| running.id)
    }

    pub fn is_cancel_requested(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|running| running.cancel_requested)
    }

    pub fn result(&self) -> Option<&ResultView> {
        self.result.as_ref()
    }

    pub fn error(&self) -> Option<&DatabaseError> {
        self.error.as_ref()
    }

    pub fn last_run(&self) -> Option<&FinishedRun> {
        self.last_run.as_ref()
    }

    /// Whether abandoning the console would discard an open transaction.
    pub fn needs_confirmation_to_close(&self) -> bool {
        self.transaction.is_open() || self.running.is_some()
    }

    /// Records that a request started. Earlier results stay visible until the
    /// new request delivers its first result.
    pub fn start(&mut self, id: RequestId, sql: String, now: Instant, wall_clock: SystemTime) {
        self.running = Some(RunningRequest {
            id,
            sql,
            started: now,
            started_at: wall_clock,
            cancel_requested: false,
        });
        self.error = None;
        self.summary = None;
        self.was_cancelled = false;
    }

    pub fn mark_cancel_requested(&mut self) {
        if let Some(running) = &mut self.running {
            running.cancel_requested = true;
        }
    }

    /// Applies one event. Events for other requests are ignored. Returns the
    /// completion when the request finished.
    pub fn apply_event(&mut self, event: RequestEvent, now: Instant) -> Option<Completion> {
        if self.running_request() != Some(event.request_id) {
            return None;
        }

        match event.kind {
            RequestEventKind::ResultStarted { result_id, columns } => {
                self.result_id = Some(result_id);
                self.result = Some(ResultView {
                    columns,
                    rows: Vec::new(),
                    summary: None,
                });
            }
            RequestEventKind::Rows { result_id, rows } => {
                if self.result_id == Some(result_id)
                    && let Some(result) = &mut self.result
                {
                    result.rows.extend(rows);
                }
            }
            RequestEventKind::StatementCompleted { result_id, summary } => {
                if result_id.is_none() {
                    // Commands without rows replace the previous grid.
                    self.result = None;
                    self.result_id = None;
                }

                if let Some(result) = &mut self.result {
                    result.summary = Some(summary.clone());
                }

                self.summary = Some(summary);
            }
            RequestEventKind::Failed(error) => self.error = Some(error),
            RequestEventKind::Cancelled => self.was_cancelled = true,
            RequestEventKind::Finished { transaction } => {
                self.transaction = transaction;
                return self.finish(now);
            }
        }

        None
    }

    fn finish(&mut self, now: Instant) -> Option<Completion> {
        let running = self.running.take()?;
        let outcome = match (&self.error, self.was_cancelled) {
            (_, true) => RunOutcome::Cancelled,
            (Some(error), false) => RunOutcome::Failed(error.clone()),
            (None, false) => RunOutcome::Succeeded,
        };
        let duration = now.saturating_duration_since(running.started);

        let run = FinishedRun {
            sql: running.sql.clone(),
            outcome: outcome.clone(),
            duration,
            summary: self.summary.clone(),
        };
        self.last_run = Some(run.clone());

        let history = self.target.as_ref().map(|target| HistoryEntry {
            connection: target.connection,
            database: target.database.clone(),
            sql: running.sql,
            started_at: running.started_at,
            duration,
            outcome: match outcome {
                RunOutcome::Succeeded => QueryOutcome::Succeeded,
                RunOutcome::Failed(_) => QueryOutcome::Failed,
                RunOutcome::Cancelled => QueryOutcome::Cancelled,
            },
            rows: self
                .summary
                .as_ref()
                .map(|summary| summary.rows_affected.unwrap_or(summary.rows_returned)),
        });

        Some(Completion { run, history })
    }

    /// The text to run: the selection when there is one, else the document.
    /// Only one statement is accepted; adapters reject more before running.
    pub fn statement_to_run<'a>(document: &'a str, selection: Option<&'a str>) -> &'a str {
        match selection {
            Some(selected) if !selected.trim().is_empty() => selected,
            _ => document,
        }
    }
}

#[cfg(test)]
mod tests {
    use qwerio_core::error::ErrorKind;
    use qwerio_core::value::Value;

    use super::*;

    fn target() -> ExecutionTarget {
        ExecutionTarget {
            connection: ConnectionId::new(),
            database: Some("app".to_owned()),
            access: AccessMode::ReadWrite,
        }
    }

    fn event(request_id: RequestId, sequence: u64, kind: RequestEventKind) -> RequestEvent {
        RequestEvent {
            request_id,
            sequence,
            kind,
        }
    }

    fn run_events(request: RequestId, result: ResultId) -> Vec<RequestEvent> {
        vec![
            event(
                request,
                0,
                RequestEventKind::ResultStarted {
                    result_id: result,
                    columns: Arc::from(vec![
                        ColumnDescriptor::new("a", "int"),
                        ColumnDescriptor::new("a", "int"),
                    ]),
                },
            ),
            event(
                request,
                1,
                RequestEventKind::Rows {
                    result_id: result,
                    rows: vec![vec![Value::Integer(1), Value::Integer(2)]],
                },
            ),
            event(
                request,
                2,
                RequestEventKind::StatementCompleted {
                    result_id: Some(result),
                    summary: StatementSummary {
                        rows_returned: 1,
                        rows_affected: None,
                        truncation: None,
                    },
                },
            ),
            event(
                request,
                3,
                RequestEventKind::Finished {
                    transaction: TransactionState::Active,
                },
            ),
        ]
    }

    #[test]
    fn a_new_console_runs_nothing() {
        let console = ConsoleState::new(Some(target()));

        assert!(!console.is_running());
        assert!(console.result().is_none());
    }

    #[test]
    fn events_build_results_and_completion_records_history() {
        let mut console = ConsoleState::new(Some(target()));
        let request = RequestId::new();
        let start = Instant::now();
        console.start(
            request,
            "SELECT 1 AS a, 2 AS a".to_owned(),
            start,
            SystemTime::now(),
        );

        let mut completion = None;
        for event in run_events(request, ResultId::new()) {
            completion = console.apply_event(event, start + Duration::from_millis(12));
        }

        let result = console.result().expect("result");
        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(console.transaction(), TransactionState::Active);

        let completion = completion.expect("finished");
        assert_eq!(completion.run.outcome, RunOutcome::Succeeded);
        assert_eq!(completion.run.duration, Duration::from_millis(12));
        assert_eq!(completion.history.map(|entry| entry.rows), Some(Some(1)));
    }

    #[test]
    fn events_from_superseded_requests_are_ignored() {
        let mut console = ConsoleState::new(Some(target()));
        let old = RequestId::new();
        let current = RequestId::new();
        console.start(
            current,
            "SELECT 2".to_owned(),
            Instant::now(),
            SystemTime::now(),
        );

        for event in run_events(old, ResultId::new()) {
            assert!(console.apply_event(event, Instant::now()).is_none());
        }

        assert!(console.result().is_none());
        assert!(console.is_running());
    }

    #[test]
    fn cancellation_is_reported_distinctly_from_success() {
        let mut console = ConsoleState::new(Some(target()));
        let request = RequestId::new();
        console.start(
            request,
            "SELECT pg_sleep(9)".to_owned(),
            Instant::now(),
            SystemTime::now(),
        );
        console.mark_cancel_requested();
        assert!(console.is_cancel_requested());

        console.apply_event(
            event(request, 0, RequestEventKind::Cancelled),
            Instant::now(),
        );
        let completion = console.apply_event(
            event(
                request,
                1,
                RequestEventKind::Finished {
                    transaction: TransactionState::Failed,
                },
            ),
            Instant::now(),
        );

        assert_eq!(
            completion.map(|completion| completion.run.outcome),
            Some(RunOutcome::Cancelled)
        );
        assert_eq!(console.transaction(), TransactionState::Failed);
        assert!(console.needs_confirmation_to_close());
    }

    #[test]
    fn failures_keep_the_error_for_display() {
        let mut console = ConsoleState::new(Some(target()));
        let request = RequestId::new();
        let error = DatabaseError::new(ErrorKind::Syntax, "syntax error").with_position(7);
        console.start(
            request,
            "SELECT FROM".to_owned(),
            Instant::now(),
            SystemTime::now(),
        );

        console.apply_event(
            event(request, 0, RequestEventKind::Failed(error.clone())),
            Instant::now(),
        );
        console.apply_event(
            event(
                request,
                1,
                RequestEventKind::Finished {
                    transaction: TransactionState::Idle,
                },
            ),
            Instant::now(),
        );

        assert_eq!(console.error(), Some(&error));
        assert_eq!(
            console.last_run().map(|run| run.outcome.clone()),
            Some(RunOutcome::Failed(error))
        );
    }

    #[test]
    fn selections_run_instead_of_the_whole_document() {
        assert_eq!(
            ConsoleState::statement_to_run("SELECT 1;\nSELECT 2", Some("SELECT 2")),
            "SELECT 2"
        );
        assert_eq!(
            ConsoleState::statement_to_run("SELECT 1", Some("  ")),
            "SELECT 1"
        );
    }
}
