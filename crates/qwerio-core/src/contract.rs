//! The engine-neutral contract suite. Each adapter's integration tests supply
//! a [`ContractFixture`] for disposable databases and run every check here,
//! so both engines are held to the same observable behavior.
//!
//! Fixtures must only create temporary databases (temporary SQLite files or
//! throwaway PostgreSQL schemas on a disposable local server).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::catalog::ObjectDetails;
use crate::connection::{ConnectionDescriptor, Credentials};
use crate::ddl::{ColumnDefinition, SchemaChange};
use crate::driver::{
    CancelOutcome, DatabaseDriver, DatabaseSession, OpenSessionRequest, QueryRequest,
};
use crate::error::ErrorKind;
use crate::filter::{Filter, FilterOperand, FilterOperator, SortKey};
use crate::paging::{PageRequest, PagingMode};
use crate::relationship::{RelationshipOutcome, follow_referenced};
use crate::resource::{CatalogEntryKind, CatalogPath, ObjectKind, ObjectRef};
use crate::result::{
    ColumnDescriptor, RequestEvent, RequestEventKind, ResultBudget, ResultStream, Row,
    TruncationReason,
};
use crate::session::{AccessMode, SessionPurpose, TransactionState};
use crate::value::Value;

/// Upper bound for any single contract step.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// Delay before cancelling a running statement.
const CANCEL_DELAY: Duration = Duration::from_millis(300);

/// Rows in the `items` fixture table.
pub const ITEM_COUNT: i64 = 25;

/// Every fifth item has a NULL name.
const NULL_NAME_EVERY: i64 = 5;

pub const WEIRD_TABLE: &str = "Weird \"Name\" Table";

/// A fresh fixture database with a connection descriptor that opens it.
pub struct FixtureDatabase {
    pub connection: ConnectionDescriptor,
    pub credentials: Credentials,
    /// The container that holds the fixture objects.
    pub path: CatalogPath,
    /// Database to request when opening sessions, for multi-database engines.
    pub database: Option<String>,
}

#[async_trait]
pub trait ContractFixture: Send + Sync {
    fn driver(&self) -> Arc<dyn DatabaseDriver>;

    /// Creates an empty disposable database.
    async fn create(&self) -> FixtureDatabase;

    /// Removes the disposable database.
    async fn destroy(&self, database: FixtureDatabase);

    /// Engine-specific DDL for the fixture tables:
    ///
    /// - `items (id integer primary key, name text, score float, big bigint)`
    /// - `orders (year int not null, number int not null, note text,
    ///   primary key (year, number))`
    /// - `lines (id integer primary key, order_no int, order_year int,
    ///   payload blob, foreign key (order_year, order_no) references orders (year, number))`
    /// - `"Weird ""Name"" Table" ("Mixed Case" integer primary key, "select" text)`
    /// - view `item_names` as `SELECT id, name FROM items`
    fn schema_statements(&self) -> Vec<String>;

    /// A literal for the given hexadecimal bytes.
    fn blob_literal(&self, hex: &str) -> String;

    /// A single statement that runs for many seconds unless cancelled.
    fn slow_query(&self) -> String;

    /// A query with one unbound parameter placeholder.
    fn parameter_query(&self) -> String;

    /// A single statement returning `count` rows with one integer column `n`.
    fn series_query(&self, count: u64) -> String;
}

/// Runs a contract check against a fresh, populated database.
pub async fn with_database<F, Fut>(fixture: &dyn ContractFixture, check: F)
where
    F: FnOnce(Context) -> Fut,
    Fut: Future<Output = ()>,
{
    let database = fixture.create().await;
    let context = Context {
        driver: fixture.driver(),
        connection: database.connection.clone(),
        credentials: database.credentials.clone(),
        path: database.path.clone(),
        database: database.database.clone(),
        fixture_sql: FixtureSql {
            slow_query: fixture.slow_query(),
            parameter_query: fixture.parameter_query(),
            series_query: fixture.series_query(100_000),
        },
    };

    populate(&context, fixture).await;
    check(context).await;
    fixture.destroy(database).await;
}

pub struct FixtureSql {
    pub slow_query: String,
    pub parameter_query: String,
    pub series_query: String,
}

/// Everything a check needs to open sessions on the fixture database.
pub struct Context {
    pub driver: Arc<dyn DatabaseDriver>,
    pub connection: ConnectionDescriptor,
    pub credentials: Credentials,
    pub path: CatalogPath,
    pub database: Option<String>,
    pub fixture_sql: FixtureSql,
}

impl Context {
    pub async fn open(
        &self,
        purpose: SessionPurpose,
        access: AccessMode,
    ) -> Arc<dyn DatabaseSession> {
        let request = OpenSessionRequest {
            connection: self.connection.clone(),
            credentials: self.credentials.clone(),
            purpose,
            access,
            database: self.database.clone(),
        };

        timed(self.driver.open(request))
            .await
            .unwrap_or_else(|error| panic!("opening a session failed: {error:?}"))
    }

    pub async fn console(&self) -> Arc<dyn DatabaseSession> {
        self.open(SessionPurpose::Console, AccessMode::ReadWrite)
            .await
    }

    pub async fn browser(&self) -> Arc<dyn DatabaseSession> {
        self.open(SessionPurpose::Browse, AccessMode::ReadOnly)
            .await
    }

    pub fn object(&self, kind: ObjectKind, name: &str) -> ObjectRef {
        ObjectRef {
            connection: self.connection.id,
            database: self.path.database.clone(),
            namespace: self.path.namespace.clone(),
            kind,
            name: name.to_owned(),
        }
    }

    pub async fn describe(&self, session: &dyn DatabaseSession, name: &str) -> Arc<ObjectDetails> {
        let object = self.object(ObjectKind::Table, name);
        let details = timed(session.describe(&object))
            .await
            .unwrap_or_else(|error| panic!("describing {name} failed: {error:?}"));

        Arc::new(details)
    }
}

async fn timed<T>(future: impl Future<Output = T>) -> T {
    match tokio::time::timeout(STEP_TIMEOUT, future).await {
        Ok(output) => output,
        Err(_) => panic!("a contract step exceeded {STEP_TIMEOUT:?}"),
    }
}

async fn populate(context: &Context, fixture: &dyn ContractFixture) {
    let session = context.console().await;

    let mut statements = fixture.schema_statements();
    statements.push(items_insert());
    statements.push(
        "INSERT INTO orders (year, number, note) VALUES \
         (2023, 1, 'first'), (2024, 1, NULL), (2024, 2, 'second'), (2024, 3, NULL)"
            .to_owned(),
    );
    statements.push(format!(
        "INSERT INTO lines (id, order_no, order_year, payload) VALUES \
         (1, 2, 2024, {}), (2, 1, 2023, NULL), (3, NULL, 2024, NULL)",
        fixture.blob_literal("00ff10")
    ));
    statements.push(
        "INSERT INTO \"Weird \"\"Name\"\" Table\" (\"Mixed Case\", \"select\") VALUES (1, 'quoted')"
            .to_owned(),
    );

    for statement in statements {
        let events = run(session.as_ref(), &statement).await;

        if let Some(error) = failure(&events) {
            panic!("fixture statement failed: {statement}\n{error:?}");
        }
    }

    close(session.as_ref()).await;
}

fn items_insert() -> String {
    let rows: Vec<String> = (1..=ITEM_COUNT)
        .map(|id| {
            let name = if id % NULL_NAME_EVERY == 0 {
                "NULL".to_owned()
            } else {
                format!("'item {id:02}'")
            };

            format!("({id}, {name}, {id}.5, {})", id * 1_000_000_000_000)
        })
        .collect();

    format!(
        "INSERT INTO items (id, name, score, big) VALUES {}",
        rows.join(", ")
    )
}

pub async fn close(session: &dyn DatabaseSession) {
    timed(session.close())
        .await
        .unwrap_or_else(|error| panic!("closing failed: {error:?}"));
}

/// Executes one statement and collects all its events.
pub async fn run(session: &dyn DatabaseSession, sql: &str) -> Vec<RequestEvent> {
    run_with_budget(session, sql, ResultBudget::DEFAULT).await
}

pub async fn run_with_budget(
    session: &dyn DatabaseSession,
    sql: &str,
    budget: ResultBudget,
) -> Vec<RequestEvent> {
    let request = QueryRequest {
        budget,
        ..QueryRequest::new(sql)
    };
    let stream = session
        .execute(request)
        .unwrap_or_else(|error| panic!("execute was rejected: {error:?}"));

    timed(collect(stream)).await
}

pub async fn collect(mut stream: ResultStream) -> Vec<RequestEvent> {
    let mut events = Vec::new();

    while let Some(event) = stream.events.recv().await {
        let is_terminal = event.kind.is_terminal();
        events.push(event);

        if is_terminal {
            break;
        }
    }

    events
}

pub fn failure(events: &[RequestEvent]) -> Option<&crate::error::DatabaseError> {
    events.iter().find_map(|event| match &event.kind {
        RequestEventKind::Failed(error) => Some(error),
        _ => None,
    })
}

pub fn columns(events: &[RequestEvent]) -> Vec<ColumnDescriptor> {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RequestEventKind::ResultStarted { columns, .. } => Some(columns.to_vec()),
            _ => None,
        })
        .unwrap_or_default()
}

pub fn rows(events: &[RequestEvent]) -> Vec<Row> {
    events
        .iter()
        .flat_map(|event| match &event.kind {
            RequestEventKind::Rows { rows, .. } => rows.clone(),
            _ => Vec::new(),
        })
        .collect()
}

pub fn final_transaction_state(events: &[RequestEvent]) -> Option<TransactionState> {
    events.last().and_then(|event| match event.kind {
        RequestEventKind::Finished { transaction } => Some(transaction),
        _ => None,
    })
}

fn assert_well_formed(events: &[RequestEvent]) {
    for (index, event) in events.iter().enumerate() {
        assert_eq!(
            event.sequence, index as u64,
            "events must be numbered in order"
        );
    }

    assert!(
        matches!(
            events.last().map(|event| &event.kind),
            Some(RequestEventKind::Finished { .. })
        ),
        "the last event must be Finished: {events:?}"
    );
}

async fn single_value(session: &dyn DatabaseSession, sql: &str) -> Value {
    let events = run(session, sql).await;
    assert_eq!(failure(&events), None, "{sql}");

    rows(&events)
        .first()
        .and_then(|row| row.first().cloned())
        .unwrap_or_else(|| panic!("no value returned by {sql}"))
}

// --- Catalog -------------------------------------------------------------

pub async fn catalog_lists_tables_and_views(context: Context) {
    let session = context.browser().await;
    let entries = timed(session.list(&context.path)).await.expect("list");

    let find = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.kind.clone())
    };

    assert_eq!(
        find("items"),
        Some(CatalogEntryKind::Object(ObjectKind::Table))
    );
    assert_eq!(
        find(WEIRD_TABLE),
        Some(CatalogEntryKind::Object(ObjectKind::Table))
    );
    assert_eq!(
        find("item_names"),
        Some(CatalogEntryKind::Object(ObjectKind::View))
    );
    close(session.as_ref()).await;
}

pub async fn catalog_describes_composite_keys_and_relationships(context: Context) {
    let session = context.browser().await;

    let orders = context.describe(session.as_ref(), "orders").await;
    let primary_key = orders.primary_key.as_ref().expect("orders primary key");
    assert_eq!(primary_key.columns, vec!["year", "number"]);
    assert!(!primary_key.allows_null);

    let lines = context.describe(session.as_ref(), "lines").await;
    let foreign_key = lines.foreign_keys.first().expect("lines foreign key");
    assert_eq!(foreign_key.columns, vec!["order_year", "order_no"]);
    assert_eq!(foreign_key.referenced_columns, vec!["year", "number"]);
    assert_eq!(foreign_key.referenced_table.name, "orders");

    let referencing = orders
        .referencing_keys
        .iter()
        .find(|key| key.table.name == "lines")
        .expect("orders is referenced by lines");
    assert_eq!(referencing.columns, vec!["order_year", "order_no"]);

    close(session.as_ref()).await;
}

pub async fn catalog_lists_relationships_in_a_container(context: Context) {
    let session = context.browser().await;
    let relationships = timed(session.relationships(&context.path))
        .await
        .expect("relationships");

    assert_eq!(relationships.len(), 1);
    let foreign_key = &relationships[0];
    assert_eq!(foreign_key.table.name, "lines");
    assert_eq!(foreign_key.columns, vec!["order_year", "order_no"]);
    assert_eq!(foreign_key.referenced_table.name, "orders");
    assert_eq!(foreign_key.referenced_columns, vec!["year", "number"]);

    close(session.as_ref()).await;
}

pub async fn catalog_reports_missing_objects(context: Context) {
    let session = context.browser().await;
    let missing = context.object(ObjectKind::Table, "does_not_exist");
    let error = timed(session.describe(&missing))
        .await
        .expect_err("missing object");

    assert_eq!(error.kind, ErrorKind::UndefinedObject);
    close(session.as_ref()).await;
}

// --- Values and results --------------------------------------------------

pub async fn results_keep_duplicate_column_names(context: Context) {
    let session = context.console().await;
    let events = run(session.as_ref(), "SELECT 1 AS a, 2 AS a").await;

    assert_well_formed(&events);
    let names: Vec<String> = columns(&events)
        .into_iter()
        .map(|column| column.name)
        .collect();
    assert_eq!(names, vec!["a", "a"]);
    assert_eq!(
        rows(&events),
        vec![vec![Value::Integer(1), Value::Integer(2)]]
    );
    close(session.as_ref()).await;
}

pub async fn values_distinguish_null_empty_and_exact_numbers(context: Context) {
    let session = context.console().await;
    let events = run(
        session.as_ref(),
        "SELECT NULL AS n, '' AS e, 9223372036854775807 AS big",
    )
    .await;

    assert_eq!(
        rows(&events),
        vec![vec![
            Value::Null,
            Value::Text(String::new()),
            Value::Integer(i64::MAX)
        ]]
    );
    close(session.as_ref()).await;
}

pub async fn values_preserve_blobs_and_floats(context: Context) {
    let session = context.console().await;

    assert_eq!(
        single_value(session.as_ref(), "SELECT payload FROM lines WHERE id = 1").await,
        Value::Bytes(vec![0x00, 0xff, 0x10])
    );
    assert_eq!(
        single_value(session.as_ref(), "SELECT score FROM items WHERE id = 3").await,
        Value::Float(3.5)
    );
    assert_eq!(
        single_value(session.as_ref(), "SELECT big FROM items WHERE id = 25").await,
        Value::Integer(25_000_000_000_000)
    );
    close(session.as_ref()).await;
}

pub async fn empty_results_report_columns_and_no_rows(context: Context) {
    let session = context.console().await;
    let events = run(session.as_ref(), "SELECT id, name FROM items WHERE 1 = 0").await;

    assert_well_formed(&events);
    assert_eq!(columns(&events).len(), 2);
    assert!(rows(&events).is_empty());
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        RequestEventKind::StatementCompleted { summary, .. } if summary.rows_returned == 0
    )));
    close(session.as_ref()).await;
}

pub async fn results_are_bounded_by_budget(context: Context) {
    let session = context.console().await;
    let budget = ResultBudget {
        max_rows: 5,
        max_bytes: usize::MAX,
        batch_rows: 2,
    };
    let events =
        run_with_budget(session.as_ref(), "SELECT id FROM items ORDER BY id", budget).await;

    assert_eq!(rows(&events).len(), 5);
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        RequestEventKind::StatementCompleted { summary, .. }
            if summary.truncation == Some(TruncationReason::RowLimit)
    )));

    // The session stays usable after truncating.
    assert_eq!(
        single_value(session.as_ref(), "SELECT 7").await,
        Value::Integer(7)
    );
    close(session.as_ref()).await;
}

pub async fn stopped_consumers_release_the_session(context: Context) {
    let session = context.console().await;
    let budget = ResultBudget {
        max_rows: 1_000_000,
        max_bytes: usize::MAX,
        batch_rows: 100,
    };
    let request = QueryRequest {
        budget,
        ..QueryRequest::new(context.fixture_sql.series_query.clone())
    };

    let mut stream = session.execute(request).expect("execute");
    let first = timed(stream.events.recv()).await;
    assert!(matches!(
        first.map(|event| event.kind),
        Some(RequestEventKind::ResultStarted { .. })
    ));
    drop(stream);

    // The producer notices the closed consumer and stops; the session then
    // accepts new work.
    let value = timed(async {
        loop {
            match session.execute(QueryRequest::new("SELECT 1")) {
                Ok(stream) => break collect(stream).await,
                Err(error) if error.kind == ErrorKind::SessionBusy => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("unexpected error: {error:?}"),
            }
        }
    })
    .await;

    assert_eq!(rows(&value), vec![vec![Value::Integer(1)]]);
    close(session.as_ref()).await;
}

// --- Statement validation -------------------------------------------------

pub async fn multiple_statements_are_rejected_before_execution(context: Context) {
    let session = context.console().await;
    let events = run(
        session.as_ref(),
        "INSERT INTO items (id, name) VALUES (1000, 'x'); SELECT 1",
    )
    .await;

    assert_well_formed(&events);
    assert_eq!(
        failure(&events).map(|error| error.kind),
        Some(ErrorKind::MultipleStatements)
    );

    let count = single_value(
        session.as_ref(),
        "SELECT count(*) FROM items WHERE id = 1000",
    )
    .await;
    assert_eq!(count, Value::Integer(0), "the first statement must not run");
    close(session.as_ref()).await;
}

pub async fn semicolons_in_strings_and_trailing_comments_are_one_statement(context: Context) {
    let session = context.console().await;

    assert_eq!(
        single_value(
            session.as_ref(),
            "SELECT 'a;b' AS text; -- trailing comment"
        )
        .await,
        Value::Text("a;b".to_owned())
    );
    assert_eq!(
        single_value(session.as_ref(), "/* leading; comment */ SELECT 2;").await,
        Value::Integer(2)
    );
    close(session.as_ref()).await;
}

pub async fn blank_statements_are_rejected(context: Context) {
    let session = context.console().await;
    let events = run(session.as_ref(), "  -- nothing here\n ; ").await;

    assert_eq!(
        failure(&events).map(|error| error.kind),
        Some(ErrorKind::EmptyStatement)
    );
    close(session.as_ref()).await;
}

pub async fn syntax_errors_report_a_position(context: Context) {
    let session = context.console().await;
    let sql = "SELECT id FROM items WHERE id = = 1";
    let events = run(session.as_ref(), sql).await;
    let error = failure(&events).expect("syntax error");

    assert_eq!(error.kind, ErrorKind::Syntax);
    let position = error.position.expect("error position").byte_offset;
    assert!(
        position <= sql.len(),
        "position {position} is inside the statement"
    );
    close(session.as_ref()).await;
}

pub async fn unbound_parameters_are_rejected(context: Context) {
    let session = context.console().await;
    let events = run(session.as_ref(), &context.fixture_sql.parameter_query).await;

    assert_eq!(
        failure(&events).map(|error| error.kind),
        Some(ErrorKind::Parameters)
    );
    close(session.as_ref()).await;
}

// --- Sessions, transactions, and cancellation -----------------------------

pub async fn transactions_stay_on_their_session(context: Context) {
    let session = context.console().await;
    let observer = context.console().await;

    assert_eq!(session.begin().await, Ok(TransactionState::Active));
    let events = run(
        session.as_ref(),
        "INSERT INTO items (id, name) VALUES (500, 'pending')",
    )
    .await;
    assert_eq!(
        final_transaction_state(&events),
        Some(TransactionState::Active)
    );

    // Uncommitted work is visible on its own session only.
    let own = single_value(
        session.as_ref(),
        "SELECT count(*) FROM items WHERE id = 500",
    )
    .await;
    assert_eq!(own, Value::Integer(1));

    assert_eq!(session.rollback().await, Ok(TransactionState::Idle));
    let after = single_value(
        observer.as_ref(),
        "SELECT count(*) FROM items WHERE id = 500",
    )
    .await;
    assert_eq!(after, Value::Integer(0));

    assert_eq!(session.begin().await, Ok(TransactionState::Active));
    run(
        session.as_ref(),
        "INSERT INTO items (id, name) VALUES (501, 'kept')",
    )
    .await;
    assert_eq!(session.commit().await, Ok(TransactionState::Idle));
    let committed = single_value(
        observer.as_ref(),
        "SELECT count(*) FROM items WHERE id = 501",
    )
    .await;
    assert_eq!(committed, Value::Integer(1));

    close(session.as_ref()).await;
    close(observer.as_ref()).await;
}

pub async fn sql_transaction_statements_update_state(context: Context) {
    let session = context.console().await;

    let begin = run(session.as_ref(), "BEGIN").await;
    assert_eq!(
        final_transaction_state(&begin),
        Some(TransactionState::Active)
    );
    assert_eq!(session.transaction_state(), TransactionState::Active);

    let rollback = run(session.as_ref(), "ROLLBACK").await;
    assert_eq!(
        final_transaction_state(&rollback),
        Some(TransactionState::Idle)
    );
    assert_eq!(session.transaction_state(), TransactionState::Idle);
    close(session.as_ref()).await;
}

pub async fn closing_rolls_back_open_transactions(context: Context) {
    let session = context.console().await;
    session.begin().await.expect("begin");
    run(
        session.as_ref(),
        "INSERT INTO items (id, name) VALUES (600, 'abandoned')",
    )
    .await;
    close(session.as_ref()).await;

    let observer = context.console().await;
    let count = single_value(
        observer.as_ref(),
        "SELECT count(*) FROM items WHERE id = 600",
    )
    .await;
    assert_eq!(count, Value::Integer(0));
    close(observer.as_ref()).await;
}

pub async fn busy_sessions_reject_concurrent_requests(context: Context) {
    let session = context.console().await;
    let stream = session
        .execute(QueryRequest::new(context.fixture_sql.slow_query.clone()))
        .expect("execute slow query");

    let second = session.execute(QueryRequest::new("SELECT 1"));
    assert_eq!(
        second.map(|_| ()).map_err(|error| error.kind),
        Err(ErrorKind::SessionBusy)
    );

    session.cancel(stream.request_id).await.expect("cancel");
    timed(collect(stream)).await;
    close(session.as_ref()).await;
}

pub async fn running_statements_can_be_cancelled(context: Context) {
    let session = context.console().await;
    let stream = session
        .execute(QueryRequest::new(context.fixture_sql.slow_query.clone()))
        .expect("execute slow query");
    let request_id = stream.request_id;

    let collector = collect(stream);
    let canceller = async {
        tokio::time::sleep(CANCEL_DELAY).await;
        session.cancel(request_id).await
    };

    let (events, outcome) = timed(async { tokio::join!(collector, canceller) }).await;

    assert_eq!(outcome, Ok(CancelOutcome::Requested));
    assert_well_formed(&events);
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, RequestEventKind::Cancelled)),
        "expected a Cancelled event: {events:?}"
    );

    // Cancellation is distinct from completion and leaves the session usable.
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, RequestEventKind::StatementCompleted { .. }))
    );
    assert_eq!(
        single_value(session.as_ref(), "SELECT 3").await,
        Value::Integer(3)
    );
    close(session.as_ref()).await;
}

pub async fn read_only_sessions_reject_writes(context: Context) {
    let session = context
        .open(SessionPurpose::Console, AccessMode::ReadOnly)
        .await;
    let events = run(
        session.as_ref(),
        "INSERT INTO items (id, name) VALUES (700, 'no')",
    )
    .await;

    assert_eq!(
        failure(&events).map(|error| error.kind),
        Some(ErrorKind::ReadOnly)
    );
    close(session.as_ref()).await;
}

// --- Table browsing --------------------------------------------------------

async fn all_pages(
    session: &dyn DatabaseSession,
    mut request: PageRequest,
) -> (Vec<Row>, PagingMode) {
    let mut rows = Vec::new();

    loop {
        let page = timed(session.fetch_page(request.clone()))
            .await
            .expect("fetch page");
        rows.extend(page.rows);

        match page.next {
            Some(next) => request.continuation = Some(next),
            None => return (rows, page.mode),
        }
    }
}

fn integer_column(rows: &[Row], index: usize) -> Vec<i64> {
    rows.iter()
        .filter_map(|row| match row.get(index) {
            Some(Value::Integer(value)) => Some(*value),
            _ => None,
        })
        .collect()
}

pub async fn pages_cover_every_row_exactly_once(context: Context) {
    let session = context.browser().await;
    let details = context.describe(session.as_ref(), "items").await;
    let request = PageRequest {
        page_size: 7,
        ..PageRequest::first(details)
    };

    let (rows, mode) = all_pages(session.as_ref(), request).await;

    assert_eq!(mode, PagingMode::Keyset);
    assert_eq!(
        integer_column(&rows, 0),
        (1..=ITEM_COUNT).collect::<Vec<_>>()
    );
    close(session.as_ref()).await;
}

pub async fn sorting_with_nulls_pages_deterministically(context: Context) {
    let session = context.browser().await;
    let details = context.describe(session.as_ref(), "items").await;
    let request = PageRequest {
        page_size: 4,
        sort: vec![SortKey::descending("name")],
        ..PageRequest::first(details)
    };

    let (rows, _) = all_pages(session.as_ref(), request).await;
    let ids = integer_column(&rows, 0);

    let mut expected: Vec<i64> = (1..=ITEM_COUNT)
        .filter(|id| id % NULL_NAME_EVERY != 0)
        .rev()
        .collect();
    expected.extend((1..=ITEM_COUNT).filter(|id| id % NULL_NAME_EVERY == 0));
    assert_eq!(ids, expected, "names descending, NULLs last, ties by id");
    close(session.as_ref()).await;
}

pub async fn filters_apply_to_the_whole_table(context: Context) {
    let session = context.browser().await;
    let details = context.describe(session.as_ref(), "items").await;

    let fetch = |filters: Vec<Filter>| {
        let request = PageRequest {
            page_size: 3,
            filters,
            ..PageRequest::first(Arc::clone(&details))
        };
        all_pages(session.as_ref(), request)
    };

    let (nulls, _) = fetch(vec![Filter::is_null("name")]).await;
    assert_eq!(integer_column(&nulls, 0), vec![5, 10, 15, 20, 25]);

    let contains = Filter::new(
        "name",
        FilterOperator::Contains,
        FilterOperand::Single(Value::Text("ITEM 1".to_owned())),
    );
    let (matching, _) = fetch(vec![contains]).await;
    assert_eq!(
        integer_column(&matching, 0),
        vec![11, 12, 13, 14, 16, 17, 18, 19]
    );

    // User-entered text is converted to the column type.
    let greater = Filter::new(
        "id",
        FilterOperator::Greater,
        FilterOperand::Single(Value::Text("22".to_owned())),
    );
    let (tail, _) = fetch(vec![greater]).await;
    assert_eq!(integer_column(&tail, 0), vec![23, 24, 25]);

    let not_equal = Filter::new(
        "name",
        FilterOperator::NotEquals,
        FilterOperand::Single(Value::Text("item 01".to_owned())),
    );
    let (others, _) = fetch(vec![not_equal]).await;
    assert_eq!(
        others.len() as i64,
        ITEM_COUNT - 1,
        "NotEquals keeps NULL names"
    );

    close(session.as_ref()).await;
}

pub async fn composite_keys_page_with_null_sort_values(context: Context) {
    let session = context.browser().await;
    let details = context.describe(session.as_ref(), "orders").await;
    let request = PageRequest {
        page_size: 1,
        sort: vec![SortKey::ascending("note")],
        ..PageRequest::first(details)
    };

    let (rows, mode) = all_pages(session.as_ref(), request).await;
    let keys: Vec<(i64, i64)> = integer_column(&rows, 0)
        .into_iter()
        .zip(integer_column(&rows, 1))
        .collect();

    assert_eq!(mode, PagingMode::Keyset);
    assert_eq!(keys, vec![(2023, 1), (2024, 2), (2024, 1), (2024, 3)]);
    close(session.as_ref()).await;
}

pub async fn views_page_with_offsets(context: Context) {
    let session = context.browser().await;
    let object = context.object(ObjectKind::View, "item_names");
    let details = Arc::new(
        timed(session.describe(&object))
            .await
            .expect("describe view"),
    );
    assert_eq!(details.object.kind, ObjectKind::View);

    let request = PageRequest {
        page_size: 10,
        sort: vec![SortKey::ascending("id")],
        ..PageRequest::first(details)
    };
    let (rows, mode) = all_pages(session.as_ref(), request).await;

    assert_eq!(mode, PagingMode::Offset);
    assert_eq!(rows.len() as i64, ITEM_COUNT);
    close(session.as_ref()).await;
}

pub async fn unusual_identifiers_are_quoted(context: Context) {
    let session = context.browser().await;
    let details = context.describe(session.as_ref(), WEIRD_TABLE).await;
    let request = PageRequest {
        sort: vec![SortKey::ascending("select")],
        filters: vec![Filter::equals("Mixed Case", Value::Integer(1))],
        ..PageRequest::first(details)
    };

    let page = timed(session.fetch_page(request))
        .await
        .expect("fetch weird table");
    let names: Vec<&str> = page
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();

    assert_eq!(names, vec!["Mixed Case", "select"]);
    assert_eq!(
        page.rows,
        vec![vec![Value::Integer(1), Value::Text("quoted".to_owned())]]
    );
    close(session.as_ref()).await;
}

pub async fn relationships_lead_to_referenced_rows(context: Context) {
    let session = context.browser().await;
    let lines = context.describe(session.as_ref(), "lines").await;
    let foreign_key = lines.foreign_keys.first().expect("foreign key").clone();

    let page = timed(session.fetch_page(PageRequest::first(Arc::clone(&lines))))
        .await
        .expect("lines");
    let first_line = page.rows.first().expect("line 1");

    let RelationshipOutcome::Target(target) = follow_referenced(&lines, first_line, &foreign_key)
    else {
        panic!("line 1 references an order");
    };

    let orders = Arc::new(
        timed(session.describe(&target.object))
            .await
            .expect("orders"),
    );
    let request = PageRequest {
        filters: target.filters,
        ..PageRequest::first(orders)
    };
    let related = timed(session.fetch_page(request))
        .await
        .expect("related orders");

    assert_eq!(related.rows.len(), 1);
    assert_eq!(related.rows[0][0], Value::Integer(2024));
    assert_eq!(related.rows[0][1], Value::Integer(2));

    let null_line = page.rows.get(2).expect("line 3");
    assert_eq!(
        follow_referenced(&lines, null_line, &foreign_key),
        RelationshipOutcome::NullKey
    );
    close(session.as_ref()).await;
}

pub async fn stale_continuations_are_rejected(context: Context) {
    let session = context.browser().await;
    let details = context.describe(session.as_ref(), "items").await;
    let first = PageRequest {
        page_size: 5,
        ..PageRequest::first(Arc::clone(&details))
    };
    let page = timed(session.fetch_page(first)).await.expect("first page");

    let changed = PageRequest {
        page_size: 5,
        sort: vec![SortKey::descending("id")],
        continuation: page.next,
        ..PageRequest::first(details)
    };
    let error = timed(session.fetch_page(changed))
        .await
        .expect_err("stale token");

    assert_eq!(error.kind, ErrorKind::InvalidContinuation);
    close(session.as_ref()).await;
}

// --- Schema changes ------------------------------------------------------

/// Tables created from the engine's suggested types get generated keys, and
/// columns can be added to them afterwards.
pub async fn schema_changes_create_tables_and_columns(context: Context) {
    let capabilities = context.driver.capabilities().clone();
    let key = ColumnDefinition {
        name: "id".to_owned(),
        data_type: capabilities.key_column_type.clone(),
        is_nullable: false,
        default: None,
        is_primary_key: true,
    };
    let typed_columns = capabilities
        .column_types
        .iter()
        .enumerate()
        .map(|(index, data_type)| ColumnDefinition {
            name: format!("column {index}"),
            data_type: data_type.clone(),
            is_nullable: true,
            default: None,
            is_primary_key: false,
        });
    let create = SchemaChange::CreateTable {
        container: context.path.clone(),
        name: "New \"Table\"".to_owned(),
        columns: std::iter::once(key).chain(typed_columns).collect(),
    };
    let table = context.object(ObjectKind::Table, "New \"Table\"");
    let add = SchemaChange::AddColumn {
        table: table.clone(),
        column: ColumnDefinition {
            name: "visits".to_owned(),
            data_type: capabilities.column_types[1].clone(),
            is_nullable: false,
            default: Some("0".to_owned()),
            is_primary_key: false,
        },
    };
    let console = context.console().await;

    for change in [&create, &add] {
        let sql = change.render().expect("render schema change");
        let events = run(console.as_ref(), &sql).await;
        assert_eq!(failure(&events), None, "{sql}");
    }

    let quoted = "\"New \"\"Table\"\"\"";
    let insert = format!("INSERT INTO {quoted} (\"column 0\") VALUES ('first')");
    assert_eq!(failure(&run(console.as_ref(), &insert).await), None);
    let select = format!("SELECT id, visits FROM {quoted}");
    assert_eq!(
        rows(&run(console.as_ref(), &select).await),
        vec![vec![Value::Integer(1), Value::Integer(0)]]
    );
    close(console.as_ref()).await;

    let browser = context.browser().await;
    let details = timed(browser.describe(&table)).await.expect("describe");
    let names: Vec<&str> = details
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    assert_eq!(names.first(), Some(&"id"));
    assert_eq!(names.last(), Some(&"visits"));
    assert_eq!(names.len(), capabilities.column_types.len() + 2);
    assert_eq!(
        details.primary_key.as_ref().map(|key| key.columns.clone()),
        Some(vec!["id".to_owned()])
    );
    close(browser.as_ref()).await;
}

/// Declares one `#[tokio::test]` per contract check for an adapter's fixture.
#[macro_export]
macro_rules! contract_tests {
    ($fixture:expr) => {
        $crate::contract_tests!(@tests $fixture;
            catalog_lists_tables_and_views,
            catalog_describes_composite_keys_and_relationships,
            catalog_lists_relationships_in_a_container,
            catalog_reports_missing_objects,
            results_keep_duplicate_column_names,
            values_distinguish_null_empty_and_exact_numbers,
            values_preserve_blobs_and_floats,
            empty_results_report_columns_and_no_rows,
            results_are_bounded_by_budget,
            stopped_consumers_release_the_session,
            multiple_statements_are_rejected_before_execution,
            semicolons_in_strings_and_trailing_comments_are_one_statement,
            blank_statements_are_rejected,
            syntax_errors_report_a_position,
            unbound_parameters_are_rejected,
            transactions_stay_on_their_session,
            sql_transaction_statements_update_state,
            closing_rolls_back_open_transactions,
            busy_sessions_reject_concurrent_requests,
            running_statements_can_be_cancelled,
            read_only_sessions_reject_writes,
            pages_cover_every_row_exactly_once,
            sorting_with_nulls_pages_deterministically,
            filters_apply_to_the_whole_table,
            composite_keys_page_with_null_sort_values,
            views_page_with_offsets,
            unusual_identifiers_are_quoted,
            relationships_lead_to_referenced_rows,
            stale_continuations_are_rejected,
            schema_changes_create_tables_and_columns,
        );
    };
    (@tests $fixture:expr; $($name:ident),* $(,)?) => {
        $(
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn $name() {
                let fixture = $fixture;
                $crate::contract::with_database(&fixture, $crate::contract::$name).await;
            }
        )*
    };
}
