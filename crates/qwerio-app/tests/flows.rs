//! End-to-end application flows with the real SQLite adapter and a temporary
//! workspace, without any GUI.

use std::path::Path;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use qwerio_app::console::{ConsoleState, ExecutionTarget, RunOutcome};
use qwerio_app::credentials::MemoryCredentialStore;
use qwerio_app::export::{ExportProgress, MAX_FILE_EXPORT_ROWS, TableExport, export_table};
use qwerio_app::local_state::LocalState;
use qwerio_app::navigation::{NavigationHistory, ViewState};
use qwerio_app::registry::DriverRegistry;
use qwerio_app::table::TableBrowserState;
use qwerio_app::{AppError, AppServices};
use qwerio_core::connection::{ConnectionOptions, EngineKind, SqliteOptions};
use qwerio_core::driver::QueryRequest;
use qwerio_core::error::ErrorKind;
use qwerio_core::export::{CsvOptions, ExportFormat, ExportTarget};
use qwerio_core::paging::PageRequest;
use qwerio_core::relationship::RelationshipOutcome;
use qwerio_core::resource::{CatalogPath, ObjectKind, ObjectRef};
use qwerio_core::session::AccessMode;
use qwerio_core::value::Value;
use qwerio_sqlite::{SqliteDriver, create_database_file};
use qwerio_workspace::Workspace;
use qwerio_workspace::explorer::{EntryTarget, ExplorerInput, Location, list_entries};
use qwerio_workspace::manifest::Placement;
use tempfile::TempDir;

struct Environment {
    _directory: TempDir,
    services: AppServices,
    workspace: Workspace,
    connection: qwerio_core::ids::ConnectionId,
}

fn setup_database(path: &Path) {
    create_database_file(path).expect("create database");
    rusqlite::Connection::open(path)
        .and_then(|connection| {
            connection.execute_batch(
                "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                 CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers (id), total NUMERIC);
                 INSERT INTO customers VALUES (1, 'Ada'), (2, 'Grace');
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 500)
                 INSERT INTO orders SELECT i, 1 + (i % 2), i * 1.5 FROM n;",
            )
        })
        .expect("seed fixture");
}

fn environment() -> Environment {
    let directory = TempDir::new().expect("temporary directory");
    let database = directory.path().join("shop.sqlite");
    setup_database(&database);

    let mut drivers = DriverRegistry::new();
    drivers.register(Arc::new(SqliteDriver::new()));
    let services = AppServices::new(
        drivers,
        LocalState::in_memory().expect("local state"),
        Arc::new(MemoryCredentialStore::default()),
    );

    let workspace_root = directory.path().join("workspace");
    std::fs::create_dir(&workspace_root).expect("workspace folder");
    let mut workspace = Workspace::create(&workspace_root, "Shop").expect("workspace");
    let connection = workspace
        .add_connection("Shop data", EngineKind::Sqlite)
        .expect("alias");
    services
        .bind_connection(
            workspace.manifest(),
            connection,
            ConnectionOptions::Sqlite(SqliteOptions::new(&database)),
            None,
        )
        .expect("bind");

    Environment {
        _directory: directory,
        services,
        workspace,
        connection,
    }
}

fn object(environment: &Environment, name: &str) -> ObjectRef {
    ObjectRef {
        connection: environment.connection,
        database: None,
        namespace: None,
        kind: ObjectKind::Table,
        name: name.to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn browse_mixed_entries_open_a_table_and_follow_a_relationship() {
    let mut environment = environment();
    let resolved = environment
        .services
        .resolve_connection(environment.workspace.manifest(), environment.connection)
        .expect("resolve");
    let session = environment
        .services
        .sessions
        .browse(&resolved.descriptor, &resolved.credentials, None)
        .await
        .expect("browse session");

    // A query saved beside the tables appears among them.
    let placement = Placement::container(environment.connection, &CatalogPath::root());
    environment
        .workspace
        .create_document(
            Path::new("big-orders.sql"),
            placement,
            "SELECT * FROM orders WHERE total > 100",
        )
        .expect("document");

    let catalog = session.list(&CatalogPath::root()).await.expect("catalog");
    let location = Location::connection_root(environment.connection);
    let entries = list_entries(&ExplorerInput {
        manifest: environment.workspace.manifest(),
        location: &location,
        catalog: Some(&catalog),
        untracked: &[],
        missing_documents: &Default::default(),
        unbound_connections: &Default::default(),
    });
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, vec!["customers", "orders", "big-orders.sql"]);

    // Open the orders table from its entry and navigate.
    let EntryTarget::Open(Location::Object(orders)) = entries[1].target.clone() else {
        panic!("orders opens a table");
    };
    let mut history = NavigationHistory::new(location.clone());
    history.visit(Location::Object(orders.clone()));

    let mut table = TableBrowserState::new(orders.clone(), ViewState::default());
    let details = Arc::new(session.describe(&orders).await.expect("describe"));
    let fetch = table.set_details(Arc::clone(&details));
    let page = session.fetch_page(fetch.request).await;
    assert!(table.apply_page(fetch.ticket, page));
    assert_eq!(table.rows().len(), 200);
    assert!(table.has_more());

    // Follow the foreign key from the first order to its customer.
    let foreign_key = details.foreign_keys.first().expect("foreign key").clone();
    let Some(RelationshipOutcome::Target(target)) = table.follow_reference(0, &foreign_key) else {
        panic!("order 1 has a customer");
    };
    history.visit_with_view(
        Location::Object(target.object.clone()),
        ViewState {
            filters: target.filters.clone(),
            ..ViewState::default()
        },
    );

    let customers = Arc::new(session.describe(&target.object).await.expect("customers"));
    let related = session
        .fetch_page(PageRequest {
            filters: target.filters,
            ..PageRequest::first(customers)
        })
        .await
        .expect("related customer");
    assert_eq!(
        related.rows,
        vec![vec![Value::Integer(2), Value::Text("Grace".to_owned())]]
    );

    // Back returns to the orders table with its view.
    let back = history.go_back().map(|entry| entry.location.clone());
    assert_eq!(back, Some(Location::Object(orders)));
}

#[tokio::test(flavor = "multi_thread")]
async fn browse_sessions_are_shared_per_connection() {
    let environment = environment();
    let resolved = environment
        .services
        .resolve_connection(environment.workspace.manifest(), environment.connection)
        .expect("resolve");

    let first = environment
        .services
        .sessions
        .browse(&resolved.descriptor, &resolved.credentials, None)
        .await
        .expect("first");
    let second = environment
        .services
        .sessions
        .browse(&resolved.descriptor, &resolved.credentials, None)
        .await
        .expect("second");

    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first.info().requested_access, AccessMode::ReadOnly);
}

#[tokio::test(flavor = "multi_thread")]
async fn console_runs_on_its_explicit_target_and_records_history() {
    let environment = environment();
    let manifest = environment.workspace.manifest();
    let resolved = environment
        .services
        .resolve_connection(manifest, environment.connection)
        .expect("resolve");
    let lease = environment
        .services
        .sessions
        .open_console(
            resolved.descriptor,
            resolved.credentials,
            None,
            AccessMode::ReadWrite,
        )
        .await
        .expect("console");

    let mut console = ConsoleState::new(Some(ExecutionTarget {
        connection: environment.connection,
        database: None,
        access: AccessMode::ReadWrite,
    }));
    assert!(!console.is_running(), "opening a console runs nothing");

    let request = QueryRequest::new("SELECT count(*) AS orders, count(*) AS orders FROM orders");
    let mut stream = lease.session().execute(request.clone()).expect("execute");
    console.start(
        request.id,
        request.sql.clone(),
        Instant::now(),
        SystemTime::now(),
    );

    let mut completion = None;
    while let Some(event) = stream.events.recv().await {
        if let Some(finished) = console.apply_event(event, Instant::now()) {
            completion = Some(finished);
        }
    }

    let completion = completion.expect("completed");
    assert_eq!(completion.run.outcome, RunOutcome::Succeeded);
    let result = console.result().expect("result");
    assert_eq!(result.columns.len(), 2, "duplicate names are kept");
    assert_eq!(
        result.rows,
        vec![vec![Value::Integer(500), Value::Integer(500)]]
    );

    let history = completion.history.expect("history entry");
    environment
        .services
        .local_state
        .record_history(manifest.id, &history, SystemTime::now())
        .expect("record");
    let recorded = environment
        .services
        .local_state
        .history(manifest.id, 10)
        .expect("history");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].connection, environment.connection);

    lease.session().close().await.expect("close");
}

#[tokio::test(flavor = "multi_thread")]
async fn unbound_and_unknown_targets_are_reported_before_running() {
    let mut environment = environment();
    let unbound = environment
        .workspace
        .add_connection("Teammate's database", EngineKind::Postgres)
        .expect("alias");

    let manifest = environment.workspace.manifest();
    assert!(matches!(
        environment.services.resolve_connection(manifest, unbound),
        Err(AppError::UnboundConnection(_))
    ));
    assert!(matches!(
        environment
            .services
            .resolve_connection(manifest, qwerio_core::ids::ConnectionId::new()),
        Err(AppError::UnknownConnection(_))
    ));
}

/// Opens a browse session on the fixture and describes `orders`.
async fn orders_session(
    environment: &Environment,
) -> (
    Arc<dyn qwerio_core::driver::DatabaseSession>,
    Arc<qwerio_core::catalog::ObjectDetails>,
) {
    let resolved = environment
        .services
        .resolve_connection(environment.workspace.manifest(), environment.connection)
        .expect("resolve");
    let session = environment
        .services
        .sessions
        .browse(&resolved.descriptor, &resolved.credentials, None)
        .await
        .expect("session");
    let details = Arc::new(
        session
            .describe(&object(environment, "orders"))
            .await
            .expect("describe"),
    );

    (session, details)
}

fn orders_export(
    details: &Arc<qwerio_core::catalog::ObjectDetails>,
    format: ExportFormat,
    max_rows: u64,
) -> TableExport {
    TableExport {
        request: PageRequest::first(Arc::clone(details)),
        format,
        target: ExportTarget::new(EngineKind::Sqlite, details),
        max_rows,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn full_exports_include_rows_beyond_the_loaded_page() {
    let environment = environment();
    let (session, details) = orders_session(&environment).await;
    let progress = ExportProgress::default();

    let mut output = Vec::new();
    let summary = export_table(
        session.as_ref(),
        orders_export(
            &details,
            ExportFormat::Csv(CsvOptions::default()),
            MAX_FILE_EXPORT_ROWS,
        ),
        &mut output,
        &progress,
    )
    .await
    .expect("export");

    let text = String::from_utf8(output).expect("utf-8");
    assert_eq!(summary.rows, 500);
    assert!(!summary.hit_limit);
    assert_eq!(progress.rows(), 500);
    assert_eq!(text.lines().count(), 501, "header plus every row");
    assert!(text.starts_with("\"id\",\"customer_id\",\"total\"\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn full_exports_stop_at_the_row_limit_with_complete_output() {
    let environment = environment();
    let (session, details) = orders_session(&environment).await;

    for (format, last_line) in [
        (ExportFormat::Json, "]"),
        (
            ExportFormat::SqlInsert,
            "INSERT INTO \"orders\" (\"id\", \"customer_id\", \"total\") VALUES (250, 1, 375);",
        ),
    ] {
        let mut output = Vec::new();
        let summary = export_table(
            session.as_ref(),
            orders_export(&details, format, 250),
            &mut output,
            &ExportProgress::default(),
        )
        .await
        .expect("export");

        let text = String::from_utf8(output).expect("utf-8");
        assert_eq!(summary.rows, 250);
        assert!(summary.hit_limit);
        assert_eq!(text.lines().last(), Some(last_line));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_exports_stop_with_a_cancelled_error() {
    let environment = environment();
    let (session, details) = orders_session(&environment).await;
    let progress = ExportProgress::default();
    progress.cancel();

    let result = export_table(
        session.as_ref(),
        orders_export(&details, ExportFormat::Json, MAX_FILE_EXPORT_ROWS),
        &mut Vec::new(),
        &progress,
    )
    .await;

    assert_eq!(
        result.map_err(|error| error.kind),
        Err(ErrorKind::Cancelled)
    );
    assert_eq!(progress.rows(), 0);
}
