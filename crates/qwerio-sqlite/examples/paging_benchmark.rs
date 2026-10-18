//! Paging benchmark fixture: builds a large temporary table and times the
//! browsing operations the UI performs. Usage:
//!
//! ```text
//! cargo run -p qwerio-sqlite --example paging_benchmark -- [rows]
//! ```
//!
//! Numbers from a debug build are only useful for comparing changes; measure
//! budgets with an optimized build on the reference hardware.

use std::sync::Arc;
use std::time::Instant;

use qwerio_core::connection::{
    ConnectionDescriptor, ConnectionOptions, Credentials, SqliteOptions,
};
use qwerio_core::driver::{DatabaseDriver, OpenSessionRequest};
use qwerio_core::filter::{Filter, FilterOperand, FilterOperator, SortKey};
use qwerio_core::ids::ConnectionId;
use qwerio_core::paging::PageRequest;
use qwerio_core::resource::{ObjectKind, ObjectRef};
use qwerio_core::session::{AccessMode, SessionPurpose};
use qwerio_core::value::Value;
use qwerio_sqlite::{SqliteDriver, create_database_file};

const DEFAULT_ROWS: u64 = 1_000_000;
const PAGES_TO_WALK: usize = 50;

fn main() {
    let rows = std::env::args()
        .nth(1)
        .and_then(|argument| argument.parse().ok())
        .unwrap_or(DEFAULT_ROWS);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(run(rows));
}

fn timed<T>(label: &str, start: Instant, value: T) -> T {
    println!(
        "{label:<40} {:>8.1} ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
    value
}

async fn run(rows: u64) {
    let directory = tempfile::TempDir::new().expect("temporary directory");
    let path = directory.path().join("benchmark.sqlite");
    create_database_file(&path).expect("create database");

    let start = Instant::now();
    rusqlite::Connection::open(&path)
        .and_then(|connection| {
            connection.execute_batch(&format!(
                "CREATE TABLE events (id INTEGER PRIMARY KEY, kind TEXT, amount REAL, note TEXT);
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {rows})
                 INSERT INTO events SELECT i, 'kind-' || (i % 17), i * 0.25,
                        CASE WHEN i % 7 = 0 THEN NULL ELSE 'note ' || i END FROM n;"
            ))
        })
        .expect("seed");
    timed(&format!("seed {rows} rows"), start, ());

    let driver = SqliteDriver::new();
    let session = driver
        .open(OpenSessionRequest {
            connection: ConnectionDescriptor {
                id: ConnectionId::new(),
                name: "benchmark".to_owned(),
                options: ConnectionOptions::Sqlite(SqliteOptions::new(&path)),
            },
            credentials: Credentials::none(),
            purpose: SessionPurpose::Browse,
            access: AccessMode::ReadOnly,
            database: None,
        })
        .await
        .expect("open");

    let object = ObjectRef {
        connection: session.info().connection,
        database: None,
        namespace: None,
        kind: ObjectKind::Table,
        name: "events".to_owned(),
    };

    let start = Instant::now();
    let details = Arc::new(session.describe(&object).await.expect("describe"));
    timed("describe", start, ());

    let mut request = PageRequest::first(Arc::clone(&details));
    let start = Instant::now();
    let mut page = session
        .fetch_page(request.clone())
        .await
        .expect("first page");
    timed("first page (primary key order)", start, ());

    let start = Instant::now();
    for _ in 0..PAGES_TO_WALK {
        request.continuation = page.next.take();
        page = session
            .fetch_page(request.clone())
            .await
            .expect("next page");
    }
    timed(&format!("{PAGES_TO_WALK} keyset pages"), start, ());

    let sorted = PageRequest {
        sort: vec![SortKey::descending("note")],
        ..PageRequest::first(Arc::clone(&details))
    };
    let start = Instant::now();
    session.fetch_page(sorted).await.expect("sorted page");
    timed("first page sorted by unindexed column", start, ());

    let filtered = PageRequest {
        filters: vec![Filter::new(
            "note",
            FilterOperator::Contains,
            FilterOperand::Single(Value::Text("99999".to_owned())),
        )],
        ..PageRequest::first(details)
    };
    let start = Instant::now();
    session.fetch_page(filtered).await.expect("filtered page");
    timed("first page with a contains filter", start, ());

    session.close().await.expect("close");
}
