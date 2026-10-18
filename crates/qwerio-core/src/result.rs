//! Result metadata, ordered request events, and retention budgets.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::error::DatabaseError;
use crate::ids::{RequestId, ResultId};
use crate::session::TransactionState;
use crate::value::Value;

/// One output column. Columns are positional: duplicate names are valid and
/// preserved, so rows are never keyed by column name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDescriptor {
    pub name: String,
    /// The engine's type name; for SQLite, the declared type (possibly empty).
    pub type_name: String,
    pub nullable: Option<bool>,
}

impl ColumnDescriptor {
    pub fn new(name: impl Into<String>, type_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
            nullable: None,
        }
    }
}

/// Positional row values matching the result's column descriptors.
pub type Row = Vec<Value>;

pub fn estimated_row_bytes(row: &Row) -> usize {
    row.iter().map(Value::estimated_bytes).sum()
}

/// Limits for rows retained from one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultBudget {
    pub max_rows: usize,
    pub max_bytes: usize,
    /// Rows per delivered batch.
    pub batch_rows: usize,
}

impl ResultBudget {
    pub const DEFAULT: Self = Self {
        max_rows: 10_000,
        max_bytes: 64 * 1024 * 1024,
        batch_rows: 250,
    };
}

impl Default for ResultBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TruncationReason {
    RowLimit,
    ByteLimit,
}

/// Tracks rows retained against a [`ResultBudget`] and batches them.
#[derive(Debug)]
pub struct BudgetTracker {
    budget: ResultBudget,
    rows: usize,
    bytes: usize,
    batch: Vec<Row>,
    truncation: Option<TruncationReason>,
}

/// What the producer should do after offering a row.
#[derive(Debug, PartialEq)]
pub enum BudgetDecision {
    Continue,
    /// A full batch is ready to deliver; keep fetching afterwards.
    Deliver(Vec<Row>),
    /// The budget is exhausted: deliver the rest and stop fetching.
    Stop(Vec<Row>),
}

impl BudgetTracker {
    pub fn new(budget: ResultBudget) -> Self {
        Self {
            budget,
            rows: 0,
            bytes: 0,
            batch: Vec::new(),
            truncation: None,
        }
    }

    /// Offers one fetched row. Rows beyond the budget are dropped and mark the
    /// result as truncated.
    pub fn offer(&mut self, row: Row) -> BudgetDecision {
        if self.truncation.is_some() {
            return BudgetDecision::Stop(Vec::new());
        }

        let row_bytes = estimated_row_bytes(&row);

        if self.rows >= self.budget.max_rows {
            self.truncation = Some(TruncationReason::RowLimit);
            return BudgetDecision::Stop(self.take_batch());
        }

        if self.bytes + row_bytes > self.budget.max_bytes {
            self.truncation = Some(TruncationReason::ByteLimit);
            return BudgetDecision::Stop(self.take_batch());
        }

        self.rows += 1;
        self.bytes += row_bytes;
        self.batch.push(row);

        if self.batch.len() >= self.budget.batch_rows.max(1) {
            return BudgetDecision::Deliver(self.take_batch());
        }

        BudgetDecision::Continue
    }

    /// Takes the pending partial batch at the end of a result.
    pub fn take_batch(&mut self) -> Vec<Row> {
        std::mem::take(&mut self.batch)
    }

    pub fn rows_retained(&self) -> usize {
        self.rows
    }

    pub fn truncation(&self) -> Option<TruncationReason> {
        self.truncation
    }
}

/// Completion summary for one statement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatementSummary {
    /// Rows delivered to the consumer.
    pub rows_returned: u64,
    /// Rows changed, for statements that report it.
    pub rows_affected: Option<u64>,
    pub truncation: Option<TruncationReason>,
}

/// One event in a request's ordered event stream.
#[derive(Clone, Debug, PartialEq)]
pub struct RequestEvent {
    pub request_id: RequestId,
    /// Strictly increasing per request, starting at zero.
    pub sequence: u64,
    pub kind: RequestEventKind,
}

/// Event grammar for one request:
///
/// ```text
/// (ResultStarted Rows* StatementCompleted)* (Failed | Cancelled)? Finished
/// ```
///
/// `Finished` is always last and carries the session's transaction state after
/// the request, so the consumer can reconcile after errors or cancellation.
/// Once `Finished` is delivered, the session accepts new requests.
#[derive(Clone, Debug, PartialEq)]
pub enum RequestEventKind {
    ResultStarted {
        result_id: ResultId,
        columns: Arc<[ColumnDescriptor]>,
    },
    Rows {
        result_id: ResultId,
        rows: Vec<Row>,
    },
    StatementCompleted {
        result_id: Option<ResultId>,
        summary: StatementSummary,
    },
    Failed(DatabaseError),
    Cancelled,
    Finished {
        transaction: TransactionState,
    },
}

impl RequestEventKind {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. })
    }
}

/// Capacity of the bounded event channel. A slow consumer pauses the producer
/// once this many events are queued.
pub const EVENT_CHANNEL_CAPACITY: usize = 8;

/// The receiving half of a request's event stream. Dropping it tells the
/// producer to stop fetching.
#[derive(Debug)]
pub struct ResultStream {
    pub request_id: RequestId,
    pub events: mpsc::Receiver<RequestEvent>,
}

/// The producing half used by adapters. It numbers events in order and
/// reports whether the consumer is still listening.
#[derive(Debug)]
pub struct EventSender {
    request_id: RequestId,
    sequence: u64,
    sender: mpsc::Sender<RequestEvent>,
}

impl EventSender {
    pub fn channel(request_id: RequestId) -> (Self, ResultStream) {
        let (sender, events) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let stream = ResultStream { request_id, events };
        let sender = Self {
            request_id,
            sequence: 0,
            sender,
        };

        (sender, stream)
    }

    pub fn request_id(&self) -> RequestId {
        self.request_id
    }

    fn next_event(&mut self, kind: RequestEventKind) -> RequestEvent {
        let event = RequestEvent {
            request_id: self.request_id,
            sequence: self.sequence,
            kind,
        };
        self.sequence += 1;
        event
    }

    /// Sends from async code. Returns `false` when the consumer has gone away.
    pub async fn send(&mut self, kind: RequestEventKind) -> bool {
        let event = self.next_event(kind);
        self.sender.send(event).await.is_ok()
    }

    /// Sends from a blocking worker thread. Must not be called inside an async
    /// runtime. Returns `false` when the consumer has gone away.
    pub fn send_blocking(&mut self, kind: RequestEventKind) -> bool {
        let event = self.next_event(kind);
        self.sender.blocking_send(event).is_ok()
    }

    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_row(text: &str) -> Row {
        vec![Value::Text(text.to_owned())]
    }

    #[test]
    fn budget_batches_rows_and_stops_at_row_limit() {
        let mut tracker = BudgetTracker::new(ResultBudget {
            max_rows: 3,
            max_bytes: usize::MAX,
            batch_rows: 2,
        });

        assert_eq!(tracker.offer(text_row("a")), BudgetDecision::Continue);
        assert_eq!(
            tracker.offer(text_row("b")),
            BudgetDecision::Deliver(vec![text_row("a"), text_row("b")])
        );
        assert_eq!(tracker.offer(text_row("c")), BudgetDecision::Continue);
        assert_eq!(
            tracker.offer(text_row("d")),
            BudgetDecision::Stop(vec![text_row("c")])
        );
        assert_eq!(tracker.truncation(), Some(TruncationReason::RowLimit));
        assert_eq!(tracker.rows_retained(), 3);
    }

    #[test]
    fn budget_stops_on_bytes_before_exceeding_limit() {
        let one_row = estimated_row_bytes(&text_row("0123456789"));
        let mut tracker = BudgetTracker::new(ResultBudget {
            max_rows: 100,
            max_bytes: one_row * 2,
            batch_rows: 100,
        });

        tracker.offer(text_row("0123456789"));
        tracker.offer(text_row("0123456789"));
        let decision = tracker.offer(text_row("0123456789"));

        assert_eq!(tracker.truncation(), Some(TruncationReason::ByteLimit));
        assert!(matches!(decision, BudgetDecision::Stop(rows) if rows.len() == 2));
    }

    #[tokio::test]
    async fn sender_numbers_events_and_detects_closed_consumer() {
        let request_id = RequestId::new();
        let (mut sender, mut stream) = EventSender::channel(request_id);

        assert!(sender.send(RequestEventKind::Cancelled).await);
        assert!(
            sender
                .send(RequestEventKind::Finished {
                    transaction: TransactionState::Idle
                })
                .await
        );

        let first = stream.events.recv().await;
        let second = stream.events.recv().await;
        assert_eq!(first.map(|event| event.sequence), Some(0));
        assert_eq!(second.map(|event| event.sequence), Some(1));

        drop(stream);
        assert!(!sender.send(RequestEventKind::Cancelled).await);
    }
}
