//! Differences that the common engine contract cannot exercise.

mod common;

use common::MysqlFixture;
use mysql_async::prelude::Queryable;
use qwerio_core::connection::{ConnectionOptions, Credentials, TlsMode};
use qwerio_core::contract::{
    Context, ContractFixture, failure, final_transaction_state, rows, run, with_database,
};
use qwerio_core::driver::OpenSessionRequest;
use qwerio_core::edit::{RowChange, RowEditRequest, RowUpdate};
use qwerio_core::error::ErrorKind;
use qwerio_core::filter::{
    Filter, FilterOperand, FilterOperator, NullsOrder, SortDirection, SortKey,
};
use qwerio_core::paging::PageRequest;
use qwerio_core::session::{AccessMode, SessionPurpose, TransactionState};
use qwerio_core::sql::ColumnRef;
use qwerio_core::value::Value;
use std::sync::Arc;

async fn execute_ok(
    session: &dyn qwerio_core::driver::DatabaseSession,
    sql: &str,
) -> Vec<qwerio_core::result::RequestEvent> {
    let events = run(session, sql).await;
    assert_eq!(failure(&events), None, "{sql}");
    events
}

#[tokio::test]
async fn ddl_commits_and_constraint_errors_keep_the_transaction_usable() {
    with_database(&MysqlFixture, |context| async move {
        let session = context.console().await;
        session.begin().await.expect("begin");
        execute_ok(session.as_ref(), "INSERT INTO items (id) VALUES (900)").await;
        let events = run(session.as_ref(), "INSERT INTO items (id) VALUES (900)").await;
        assert_eq!(
            failure(&events).map(|error| error.kind),
            Some(ErrorKind::Constraint)
        );
        assert_eq!(
            final_transaction_state(&events),
            Some(TransactionState::Active)
        );
        let events = execute_ok(session.as_ref(), "CREATE TABLE committed_ddl (id int)").await;
        assert_eq!(
            final_transaction_state(&events),
            Some(TransactionState::Idle)
        );
        session.rollback().await.expect("rollback after DDL");
        let observer = context.console().await;
        assert_eq!(
            rows(
                &execute_ok(
                    observer.as_ref(),
                    "SELECT count(*) FROM items WHERE id = 900"
                )
                .await
            ),
            vec![vec![Value::Integer(1)]]
        );
        session.close().await.expect("close");
        observer.close().await.expect("close observer");
    })
    .await;
}

#[tokio::test]
async fn auto_increment_defaults_generated_columns_and_key_changes_are_read_back() {
    with_database(&MysqlFixture, |context| async move {
        let session = context.console().await;
        execute_ok(session.as_ref(), "CREATE TABLE generated_items (id bigint unsigned AUTO_INCREMENT PRIMARY KEY, name varchar(255) DEFAULT 'default name', upper_name varchar(255) GENERATED ALWAYS AS (UPPER(name)) STORED) ENGINE=InnoDB").await;
        let details = context.describe(session.as_ref(), "generated_items").await;
        assert!(details.column("upper_name").expect("generated column").generated);
        assert!(!details.column("id").expect("identity").generated);
        let saved = session.apply_row_edits(RowEditRequest { details: Arc::clone(&details), changes: vec![RowChange::Insert { values: Vec::new() }] }).await.expect("insert defaults");
        assert_eq!(saved[0].values, vec![Value::Integer(1), Value::Text("default name".to_owned()), Value::Text("DEFAULT NAME".to_owned())]);
        let change = RowChange::Update(RowUpdate {
            identity: vec![(ColumnRef::Column("id".to_owned()), Value::Integer(1))],
            changes: vec![("id".to_owned(), Value::Decimal("18446744073709551615".to_owned())), ("name".to_owned(), Value::Text("Changed".to_owned()))],
        });
        let saved = session.apply_row_edits(RowEditRequest { details: Arc::clone(&details), changes: vec![change] }).await.expect("update key");
        assert_eq!(saved[0].values[0], Value::Decimal("18446744073709551615".to_owned()));
        assert_eq!(saved[0].values[2], Value::Text("CHANGED".to_owned()));
        let page = session.fetch_page(PageRequest::first(details)).await.expect("browse");
        assert_eq!(page.rows[0], saved[0].values);
        session.close().await.expect("close");
    }).await;
}

#[tokio::test]
async fn unchanged_updates_succeed_and_nontransactional_edits_are_rejected() {
    with_database(&MysqlFixture, |context| async move {
        let session = context.console().await;
        let details = context.describe(session.as_ref(), "items").await;
        let saved = session
            .apply_row_edits(RowEditRequest {
                details,
                changes: vec![RowChange::Update(RowUpdate {
                    identity: vec![(ColumnRef::Column("id".to_owned()), Value::Integer(1))],
                    changes: vec![("name".to_owned(), Value::Text("item 01".to_owned()))],
                })],
            })
            .await
            .expect("unchanged update");
        assert_eq!(saved[0].values[0], Value::Integer(1));
        execute_ok(
            session.as_ref(),
            "CREATE TABLE nontransactional (id int PRIMARY KEY) ENGINE=MyISAM",
        )
        .await;
        let details = context.describe(session.as_ref(), "nontransactional").await;
        let error = session
            .apply_row_edits(RowEditRequest {
                details,
                changes: vec![RowChange::Insert {
                    values: vec![("id".to_owned(), Value::Integer(1))],
                }],
            })
            .await
            .expect_err("must not offer partial atomic saves");
        assert_eq!(error.kind, ErrorKind::Unsupported);
        session.close().await.expect("close");
    })
    .await;
}

#[tokio::test]
async fn decimal_binary_and_temporal_values_keep_precision() {
    with_database(&MysqlFixture, |context| async move {
        let session = context.console().await;
        execute_ok(session.as_ref(), "CREATE TABLE typed_values (id int PRIMARY KEY, exact_value decimal(40,10), binary_value varbinary(8), timestamp_value datetime(6), duration time(6))").await;
        execute_ok(session.as_ref(), "INSERT INTO typed_values VALUES (1, 123456789012345678901234567890.1234567890, X'00ff80', '2025-01-02 03:04:05.123456', '-100:02:03.123456')").await;
        let events = execute_ok(session.as_ref(), "SELECT * FROM typed_values").await;
        let values = &rows(&events)[0];
        assert_eq!(values[1], Value::Decimal("123456789012345678901234567890.1234567890".to_owned()));
        assert_eq!(values[2], Value::Bytes(vec![0, 255, 128]));
        assert_eq!(values[3].canonical_text().as_deref(), Some("2025-01-02 03:04:05.123456"));
        assert_eq!(values[4].canonical_text().as_deref(), Some("-100:02:03.123456"));
        session.close().await.expect("close");
    }).await;
}

#[tokio::test]
async fn filters_escape_wildcards_and_fold_unicode_with_binary_collations() {
    with_database(&MysqlFixture, |context| async move {
        let session = context.console().await;
        execute_ok(
            session.as_ref(),
            "INSERT INTO items (id, name) VALUES (100, 'Äpfel_%'), (101, 'äPFELxy')",
        )
        .await;
        let details = context.describe(session.as_ref(), "items").await;
        let request = PageRequest {
            filters: vec![Filter {
                column: "name".to_owned(),
                operator: FilterOperator::Contains,
                operand: FilterOperand::Single(Value::Text("äPFEL_%".to_owned())),
            }],
            ..PageRequest::first(details)
        };
        let page = session.fetch_page(request).await.expect("unicode filter");
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0][0], Value::Integer(100));
        session.close().await.expect("close");
    })
    .await;
}

#[tokio::test]
async fn explicit_nulls_first_pages_without_duplicates() {
    with_database(&MysqlFixture, |context| async move {
        let session = context.browser().await;
        let details = context.describe(session.as_ref(), "items").await;
        let mut request = PageRequest {
            sort: vec![SortKey {
                column: "name".to_owned(),
                direction: SortDirection::Ascending,
                nulls: NullsOrder::First,
            }],
            page_size: 3,
            ..PageRequest::first(details)
        };
        let mut ids = Vec::new();
        loop {
            let page = session.fetch_page(request.clone()).await.expect("page");
            ids.extend(page.rows.iter().map(|row| row[0].clone()));
            let Some(token) = page.next else {
                break;
            };
            request.continuation = Some(token);
        }
        assert_eq!(
            &ids[..5],
            &[
                Value::Integer(5),
                Value::Integer(10),
                Value::Integer(15),
                Value::Integer(20),
                Value::Integer(25)
            ]
        );
        assert_eq!(ids.len(), 25);
        session.close().await.expect("close");
    })
    .await;
}

#[tokio::test]
async fn begin_with_comments_works_and_extra_statements_never_run() {
    with_database(&MysqlFixture, |context| async move {
        let session = context.console().await;
        let begin = execute_ok(session.as_ref(), "/* note */ BEGIN WORK; -- done").await;
        assert_eq!(
            final_transaction_state(&begin),
            Some(TransactionState::Active)
        );
        session.rollback().await.expect("rollback");
        let events = run(
            session.as_ref(),
            "BEGIN; INSERT INTO items (id) VALUES (999)",
        )
        .await;
        assert!(failure(&events).is_some());
        let events = execute_ok(
            session.as_ref(),
            "SELECT count(*) FROM items WHERE id = 999",
        )
        .await;
        assert_eq!(rows(&events), vec![vec![Value::Integer(0)]]);
        session.close().await.expect("close");
    })
    .await;
}

#[tokio::test]
async fn connection_errors_keep_passwords_private_and_validate_tls() {
    let fixture = MysqlFixture;
    let database = fixture.create().await;
    let driver = fixture.driver();
    let request = OpenSessionRequest {
        connection: database.connection.clone(),
        credentials: Credentials::password("wrong-private-password"),
        purpose: SessionPurpose::Console,
        access: AccessMode::ReadWrite,
        database: database.database.clone(),
    };
    let error = driver
        .open(request.clone())
        .await
        .err()
        .expect("wrong password");
    assert_eq!(error.kind, ErrorKind::Authentication);
    assert!(!format!("{error:?}").contains("wrong-private-password"));
    let mut request = OpenSessionRequest {
        credentials: database.credentials.clone(),
        ..request
    };
    let (ConnectionOptions::Mysql(options) | ConnectionOptions::Mariadb(options)) =
        &mut request.connection.options
    else {
        panic!("mysql options");
    };
    options.tls = TlsMode::VerifyFull { ca_file: None };
    let error = driver
        .open(request)
        .await
        .err()
        .expect("self-signed CA must not be trusted by default");
    assert_eq!(error.kind, ErrorKind::Tls);
    fixture.destroy(database).await;
}

#[tokio::test]
async fn a_lost_connection_never_reconnects_silently() {
    with_database(&MysqlFixture, |context: Context| async move {
        let session = context.console().await;
        let events = execute_ok(session.as_ref(), "SELECT CONNECTION_ID()").await;
        let id = rows(&events)[0][0].canonical_text().expect("connection id");
        let mut admin = MysqlFixture.admin().await;
        admin
            .query_drop(format!("KILL CONNECTION {id}"))
            .await
            .expect("kill connection");
        admin.disconnect().await.expect("disconnect admin");
        let events = run(session.as_ref(), "SELECT 1").await;
        assert_eq!(
            failure(&events).map(|error| error.kind),
            Some(ErrorKind::Connection)
        );
        assert_eq!(
            final_transaction_state(&events),
            Some(TransactionState::Unknown)
        );
        let events = run(session.as_ref(), "SELECT 2").await;
        assert!(failure(&events).is_some());
        // A broken connection may report its close failure; it must still be released.
        let _closed = session.close().await;
    })
    .await;
}

#[tokio::test]
async fn generated_text_keys_are_evaluated_once_and_returned() {
    with_database(&MysqlFixture, |context| async move {
        let session = context.console().await;
        execute_ok(session.as_ref(), "CREATE TABLE text_keys (id varchar(36) PRIMARY KEY DEFAULT (UUID()), name varchar(255)) ENGINE=InnoDB").await;
        let details = context.describe(session.as_ref(), "text_keys").await;
        let saved = session.apply_row_edits(RowEditRequest { details: Arc::clone(&details), changes: vec![RowChange::Insert { values: vec![("name".to_owned(), Value::Text("generated".to_owned()))] }] }).await.expect("insert generated text key");
        let id = saved[0].values[0].canonical_text().expect("generated UUID");
        assert_eq!(id.len(), 36);
        let page = session.fetch_page(PageRequest::first(details)).await.expect("page");
        assert_eq!(page.rows[0], saved[0].values);
        session.close().await.expect("close");
    }).await;
}

#[tokio::test]
async fn literal_and_column_dependent_key_defaults_are_read_back() {
    with_database(&MysqlFixture, |context| async move {
        let session = context.console().await;
        execute_ok(session.as_ref(), "CREATE TABLE literal_keys (id varchar(36) PRIMARY KEY DEFAULT 'fixed', name text) ENGINE=InnoDB").await;
        let details = context.describe(session.as_ref(), "literal_keys").await;
        let saved = session.apply_row_edits(RowEditRequest { details, changes: vec![RowChange::Insert { values: Vec::new() }] }).await.expect("literal key");
        assert_eq!(saved[0].values[0], Value::Text("fixed".to_owned()));
        execute_ok(session.as_ref(), "CREATE TABLE dependent_keys (seed int NOT NULL, id int PRIMARY KEY DEFAULT (seed + 1)) ENGINE=InnoDB").await;
        let details = context.describe(session.as_ref(), "dependent_keys").await;
        let saved = session.apply_row_edits(RowEditRequest { details, changes: vec![RowChange::Insert { values: vec![("seed".to_owned(), Value::Integer(41))] }] }).await.expect("dependent key");
        assert_eq!(saved[0].values, vec![Value::Integer(41), Value::Integer(42)]);
        execute_ok(session.as_ref(), "CREATE TABLE decimal_keys (seed decimal(40,0) NOT NULL, id decimal(40,0) PRIMARY KEY DEFAULT (seed + 1)) ENGINE=InnoDB").await;
        let details = context.describe(session.as_ref(), "decimal_keys").await;
        let saved = session.apply_row_edits(RowEditRequest { details, changes: vec![RowChange::Insert { values: vec![("seed".to_owned(), Value::Decimal("9007199254740993".to_owned()))] }] }).await.expect("exact dependent key");
        assert_eq!(saved[0].values, vec![Value::Decimal("9007199254740993".to_owned()), Value::Decimal("9007199254740994".to_owned())]);
        session.close().await.expect("close");
    }).await;
}

#[tokio::test]
async fn connections_without_a_default_database_can_list_and_create_databases() {
    use qwerio_core::ddl::SchemaChange;
    use qwerio_core::resource::{CatalogEntryKind, CatalogPath};
    let fixture = MysqlFixture;
    let database = fixture.create().await;
    let driver = fixture.driver();
    let session = driver
        .open(OpenSessionRequest {
            connection: fixture.descriptor(""),
            credentials: fixture.credentials(),
            purpose: SessionPurpose::Console,
            access: AccessMode::ReadWrite,
            database: None,
        })
        .await
        .expect("connect without database");
    let entries = session
        .list(&CatalogPath::root())
        .await
        .expect("list databases");
    let original = database.database.as_deref().expect("database");
    assert!(
        entries
            .iter()
            .any(|entry| entry.kind == CatalogEntryKind::Database && entry.name == original)
    );
    let name = format!(
        "qwerio_test_{}",
        qwerio_core::ids::ConnectionId::new().as_uuid().simple()
    );
    let change = SchemaChange::CreateDatabase { name: name.clone() };
    execute_ok(
        session.as_ref(),
        &change.render_for_engine(fixture.engine()).expect("DDL"),
    )
    .await;
    execute_ok(
        session.as_ref(),
        &format!("DROP DATABASE {}", fixture.engine().quote_identifier(&name)),
    )
    .await;
    session.close().await.expect("close");
    fixture.destroy(database).await;
}

#[tokio::test]
async fn large_numeric_keys_page_and_filter_without_rounding() {
    with_database(&MysqlFixture, |context| async move {
        let session = context.console().await;
        execute_ok(session.as_ref(), "CREATE TABLE large_keys (id bigint unsigned PRIMARY KEY, exact_value decimal(40,0) NOT NULL UNIQUE) ENGINE=InnoDB").await;
        execute_ok(session.as_ref(), "INSERT INTO large_keys VALUES (18446744073709551614, 9007199254740993), (18446744073709551615, 9007199254740994)").await;
        let details = context.describe(session.as_ref(), "large_keys").await;
        for column in ["id", "exact_value"] {
            let first = PageRequest { page_size: 1, sort: vec![SortKey::ascending(column)], ..PageRequest::first(Arc::clone(&details)) };
            let page = session.fetch_page(first.clone()).await.expect("first exact page");
            assert_eq!(page.rows[0][0], Value::Decimal("18446744073709551614".to_owned()));
            let second = PageRequest { continuation: page.next, ..first };
            let page = session.fetch_page(second).await.expect("second exact page");
            assert_eq!(page.rows[0][0], Value::Decimal("18446744073709551615".to_owned()));
        }
        let request = PageRequest { filters: vec![Filter::equals("id", Value::Decimal("18446744073709551614".to_owned()))], ..PageRequest::first(details) };
        let page = session.fetch_page(request).await.expect("exact filter");
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0][0], Value::Decimal("18446744073709551614".to_owned()));
        session.close().await.expect("close");
    }).await;
}
