//! PostgreSQL behavior beyond the shared contract.

mod common;

use std::sync::Arc;

use common::TestServer;
use qwerio_core::connection::{ConnectionDescriptor, ConnectionOptions, Credentials, TlsMode};
use qwerio_core::contract::{close, failure, final_transaction_state, rows, run};
use qwerio_core::driver::{DatabaseDriver, DatabaseSession, OpenSessionRequest};
use qwerio_core::error::ErrorKind;
use qwerio_core::ids::ConnectionId;
use qwerio_core::paging::PageRequest;
use qwerio_core::resource::{CatalogEntryKind, CatalogPath, EntryAccess, ObjectKind, ObjectRef};
use qwerio_core::session::{AccessMode, SessionPurpose, TransactionState};
use qwerio_core::value::{UnsupportedValue, Value};
use qwerio_postgres::PostgresDriver;

struct TestDatabase {
    server: TestServer,
    name: String,
}

impl TestDatabase {
    async fn new() -> Self {
        let server = TestServer::from_env();
        let name = server.create_database().await;
        Self { server, name }
    }

    fn request(&self, purpose: SessionPurpose, access: AccessMode) -> OpenSessionRequest {
        OpenSessionRequest {
            connection: self.server.descriptor(&self.name),
            credentials: self.server.credentials(),
            purpose,
            access,
            database: None,
        }
    }

    async fn open(&self, purpose: SessionPurpose, access: AccessMode) -> Arc<dyn DatabaseSession> {
        driver()
            .open(self.request(purpose, access))
            .await
            .expect("open session")
    }

    async fn console(&self) -> Arc<dyn DatabaseSession> {
        self.open(SessionPurpose::Console, AccessMode::ReadWrite)
            .await
    }

    async fn drop(self) {
        self.server.drop_database(&self.name).await;
    }
}

fn driver() -> PostgresDriver {
    PostgresDriver::new(tokio::runtime::Handle::current())
}

async fn values(session: &dyn DatabaseSession, sql: &str) -> Vec<Value> {
    let events = run(session, sql).await;
    assert_eq!(failure(&events), None, "{sql}");
    rows(&events).into_iter().next().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_inside_transactions_mark_them_failed_until_rollback() {
    let database = TestDatabase::new().await;
    let session = database.console().await;

    session.begin().await.expect("begin");
    let division = run(session.as_ref(), "SELECT 1 / 0").await;
    assert_eq!(
        final_transaction_state(&division),
        Some(TransactionState::Failed)
    );

    let blocked = run(session.as_ref(), "SELECT 1").await;
    assert_eq!(
        failure(&blocked).map(|error| error.kind),
        Some(ErrorKind::TransactionAborted)
    );

    assert_eq!(session.rollback().await, Ok(TransactionState::Idle));
    assert_eq!(
        values(session.as_ref(), "SELECT 1").await,
        vec![Value::Integer(1)]
    );
    close(session.as_ref()).await;
    database.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_inside_a_transaction_fails_it() {
    let database = TestDatabase::new().await;
    let session = database.console().await;
    session.begin().await.expect("begin");

    let stream = session
        .execute(qwerio_core::driver::QueryRequest::new(
            "SELECT pg_sleep(60)",
        ))
        .expect("execute");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    session.cancel(stream.request_id).await.expect("cancel");
    let events = qwerio_core::contract::collect(stream).await;

    assert_eq!(
        final_transaction_state(&events),
        Some(TransactionState::Failed)
    );
    session.rollback().await.expect("rollback");
    close(session.as_ref()).await;
    database.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn numerics_and_big_values_stay_exact() {
    let database = TestDatabase::new().await;
    let session = database.console().await;

    let decoded = values(
        session.as_ref(),
        "SELECT 123456789012345678901234567890.000000000000000000001::numeric, \
                'NaN'::numeric, -0.5::numeric(10,3), 0.1::real, 9223372036854775807::bigint",
    )
    .await;

    assert_eq!(
        decoded,
        vec![
            Value::Decimal("123456789012345678901234567890.000000000000000000001".to_owned()),
            Value::Decimal("NaN".to_owned()),
            Value::Decimal("-0.500".to_owned()),
            Value::Float(0.1),
            Value::Integer(i64::MAX),
        ]
    );
    close(session.as_ref()).await;
    database.drop().await;
}

/// Decoded values must match PostgreSQL's own output function exactly
/// (`format('%s', ...)` uses it; some `::text` casts differ, such as inet).
#[tokio::test(flavor = "multi_thread")]
async fn decoded_values_match_server_text() {
    let database = TestDatabase::new().await;
    let session = database.console().await;
    run(session.as_ref(), "SET TimeZone = 'UTC'").await;
    run(session.as_ref(), "SET IntervalStyle = 'postgres'").await;

    let expressions = [
        "'2024-02-29 13:45:01.5'::timestamp",
        "'0044-03-15 BC'::date",
        "'2024-02-29 13:45:01.123456+02'::timestamptz",
        "'23:59:59.000001'::time",
        "'12:00:00+05:30'::timetz",
        "'1 year 2 months 3 days 04:05:06.7'::interval",
        "'-3 days -00:00:01'::interval",
        "'12345.6789'::numeric",
        "'-0.000123'::numeric",
        "'192.168.0.0/16'::cidr",
        "'2001:db8::1'::inet",
        "'08:00:2b:01:02:03'::macaddr",
        "'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid",
        "'{\"a\": [1, 2]}'::jsonb",
        "'infinity'::timestamp",
    ];

    for expression in expressions {
        let sql = format!("SELECT {expression}, format('%s', {expression})");
        let decoded = values(session.as_ref(), &sql).await;
        let canonical = decoded[0].canonical_text();
        let server_text = match &decoded[1] {
            Value::Text(text) => Some(text.clone()),
            other => panic!("expected text, got {other:?}"),
        };

        assert_eq!(canonical, server_text, "{expression}");
    }

    close(session.as_ref()).await;
    database.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn arrays_enums_and_domains_decode_through_their_base_types() {
    let database = TestDatabase::new().await;
    let session = database.console().await;
    run(
        session.as_ref(),
        "CREATE TYPE mood AS ENUM ('happy', 'sad')",
    )
    .await;
    run(
        session.as_ref(),
        "CREATE DOMAIN positive AS integer CHECK (VALUE > 0)",
    )
    .await;

    let decoded = values(
        session.as_ref(),
        "SELECT ARRAY[1, NULL, 3], 'sad'::mood, 7::positive, ARRAY['x', 'y\"z']",
    )
    .await;

    assert_eq!(
        decoded,
        vec![
            Value::Array(vec![Value::Integer(1), Value::Null, Value::Integer(3)]),
            Value::Text("sad".to_owned()),
            Value::Integer(7),
            Value::Array(vec![
                Value::Text("x".to_owned()),
                Value::Text("y\"z".to_owned())
            ]),
        ]
    );
    close(session.as_ref()).await;
    database.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_types_keep_raw_bytes_in_queries_and_text_when_browsing() {
    let database = TestDatabase::new().await;
    let session = database.console().await;

    let decoded = values(session.as_ref(), "SELECT point(1, 2)").await;
    assert!(matches!(
        &decoded[0],
        Value::Unsupported(UnsupportedValue { type_name, text: None, raw: Some(_) }) if type_name == "point"
    ));

    run(
        session.as_ref(),
        "CREATE TABLE shapes (id integer PRIMARY KEY, at point)",
    )
    .await;
    run(
        session.as_ref(),
        "INSERT INTO shapes VALUES (1, point(1, 2))",
    )
    .await;

    let object = ObjectRef {
        connection: session.info().connection,
        database: Some(database.name.clone()),
        namespace: Some("public".to_owned()),
        kind: ObjectKind::Table,
        name: "shapes".to_owned(),
    };
    let details = Arc::new(session.describe(&object).await.expect("describe"));
    assert!(!details.columns[1].decodable);

    let page = session
        .fetch_page(PageRequest::first(details))
        .await
        .expect("fetch page");
    assert_eq!(
        page.rows[0][1],
        Value::Unsupported(UnsupportedValue {
            type_name: "point".to_owned(),
            text: Some("(1,2)".to_owned()),
            raw: None,
        })
    );
    close(session.as_ref()).await;
    database.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn catalog_separates_databases_and_schemas() {
    let database = TestDatabase::new().await;
    let session = database
        .open(SessionPurpose::Browse, AccessMode::ReadOnly)
        .await;

    let databases = session.list(&CatalogPath::root()).await.expect("databases");
    assert!(
        databases
            .iter()
            .any(|entry| entry.name == database.name && entry.kind == CatalogEntryKind::Database)
    );

    let schemas = session
        .list(&CatalogPath::database(database.name.clone()))
        .await
        .expect("schemas");
    assert!(schemas.iter().any(|entry| {
        entry.name == "public" && entry.kind == CatalogEntryKind::Namespace { is_system: false }
    }));
    assert!(schemas.iter().any(|entry| {
        entry.name == "pg_catalog" && entry.kind == CatalogEntryKind::Namespace { is_system: true }
    }));

    let other = session.list(&CatalogPath::database("postgres")).await;
    assert_eq!(
        other.err().map(|error| error.kind),
        Some(ErrorKind::InvalidRequest)
    );
    close(session.as_ref()).await;
    database.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn objects_without_privileges_are_listed_as_inaccessible() {
    let database = TestDatabase::new().await;
    let role = format!("reader_{}", ConnectionId::new().as_uuid().simple());
    let admin = database.console().await;

    for statement in [
        "CREATE TABLE secret (id integer)".to_owned(),
        "CREATE TABLE open_data (id integer)".to_owned(),
        format!("CREATE ROLE {role} LOGIN PASSWORD 'reader-password'"),
        format!("GRANT SELECT ON open_data TO {role}"),
    ] {
        let events = run(admin.as_ref(), &statement).await;
        assert_eq!(failure(&events), None, "{statement}");
    }

    let mut request = database.request(SessionPurpose::Browse, AccessMode::ReadOnly);
    if let ConnectionOptions::Postgres(options) = &mut request.connection.options {
        options.user = role.clone();
    }
    request.credentials = Credentials::password("reader-password");
    let reader = driver().open(request).await.expect("open as reader");

    let objects = reader
        .list(&CatalogPath::namespace(
            Some(database.name.clone()),
            "public",
        ))
        .await
        .expect("objects");
    let access = |name: &str| {
        objects
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.access)
    };

    assert_eq!(access("secret"), Some(EntryAccess::NoPrivilege));
    assert_eq!(access("open_data"), Some(EntryAccess::Accessible));

    let denied = run(reader.as_ref(), "SELECT * FROM secret").await;
    assert_eq!(
        failure(&denied).map(|error| error.kind),
        Some(ErrorKind::Permission)
    );

    close(reader.as_ref()).await;
    run(admin.as_ref(), &format!("DROP OWNED BY {role}")).await;
    close(admin.as_ref()).await;
    database.drop().await;
    let cleanup = database_cleanup_role(&role).await;
    assert!(cleanup, "role cleanup");
}

async fn database_cleanup_role(role: &str) -> bool {
    TestServer::from_env()
        .admin()
        .await
        .batch_execute(&format!("DROP ROLE IF EXISTS {role}"))
        .await
        .is_ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_passwords_fail_authentication_without_echoing_secrets() {
    let database = TestDatabase::new().await;
    let mut request = database.request(SessionPurpose::Browse, AccessMode::ReadOnly);
    request.credentials = Credentials::password("definitely-wrong-secret");

    let error = driver()
        .open(request)
        .await
        .err()
        .expect("authentication fails");

    assert_eq!(error.kind, ErrorKind::Authentication);
    assert!(!format!("{error:?}").contains("definitely-wrong-secret"));
    database.drop().await;
}

fn descriptor_with_tls(server: &TestServer, host: &str, tls: TlsMode) -> ConnectionDescriptor {
    let mut options = server.options("postgres");
    options.host = host.to_owned();
    options.tls = tls;

    ConnectionDescriptor {
        id: ConnectionId::new(),
        name: "tls".to_owned(),
        options: ConnectionOptions::Postgres(options),
    }
}

async fn open_with(server: &TestServer, connection: ConnectionDescriptor) -> Result<(), ErrorKind> {
    let request = OpenSessionRequest {
        connection,
        credentials: server.credentials(),
        purpose: SessionPurpose::Browse,
        access: AccessMode::ReadOnly,
        database: None,
    };

    match driver().open(request).await {
        Ok(session) => {
            close(session.as_ref()).await;
            Ok(())
        }
        Err(error) => Err(error.kind),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_validates_the_certificate_chain_and_host_name() {
    let server = TestServer::from_env();
    let with_test_ca = TlsMode::VerifyFull {
        ca_file: Some(server.ca_file.clone()),
    };

    let trusted = descriptor_with_tls(&server, "localhost", with_test_ca.clone());
    assert_eq!(open_with(&server, trusted).await, Ok(()));

    // The throwaway CA is not in the platform trust store.
    let untrusted =
        descriptor_with_tls(&server, "localhost", TlsMode::VerifyFull { ca_file: None });
    assert_eq!(open_with(&server, untrusted).await, Err(ErrorKind::Tls));

    // The certificate names localhost, not the IP address.
    let wrong_name = descriptor_with_tls(&server, "127.0.0.1", with_test_ca);
    assert_eq!(open_with(&server, wrong_name).await, Err(ErrorKind::Tls));
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_sessions_report_the_weaker_enforcement() {
    let database = TestDatabase::new().await;
    let session = database
        .open(SessionPurpose::Console, AccessMode::ReadOnly)
        .await;

    assert_eq!(
        session.info().enforcement,
        qwerio_core::session::ReadOnlyEnforcement::TransactionDefault
    );
    assert!(!session.info().enforcement.guarantees_no_writes());
    close(session.as_ref()).await;
    database.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rendered_create_database_runs_outside_a_transaction() {
    let database = TestDatabase::new().await;
    let created = format!("qwerio_test_{}", ConnectionId::new().as_uuid().simple());
    let change = qwerio_core::ddl::SchemaChange::CreateDatabase {
        name: created.clone(),
    };
    let session = database.console().await;

    let sql = change.render().expect("render");
    let events = run(session.as_ref(), &sql).await;
    assert_eq!(failure(&events), None, "{sql}");

    let databases = session.list(&CatalogPath::root()).await.expect("databases");
    assert!(databases.iter().any(|entry| entry.name == created));
    close(session.as_ref()).await;

    database.server.drop_database(&created).await;
    database.drop().await;
}
