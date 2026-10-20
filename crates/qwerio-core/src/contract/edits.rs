//! Contract checks for row edits from the table browser.

use super::*;
use crate::edit::{RowChange, RowEditRequest, RowUpdate};
use crate::sql::ColumnRef;

fn key(column: &str, value: Value) -> (ColumnRef, Value) {
    (ColumnRef::Column(column.to_owned()), value)
}

fn change(column: &str, value: Value) -> (String, Value) {
    (column.to_owned(), value)
}

async fn item_rows(context: &Context, id: i64) -> Vec<Row> {
    let session = context.browser().await;
    let details = context.describe(session.as_ref(), "items").await;
    let request = PageRequest {
        filters: vec![Filter::equals("id", Value::Integer(id))],
        ..PageRequest::first(details)
    };
    let page = timed(session.fetch_page(request))
        .await
        .expect("fetch item");
    close(session.as_ref()).await;

    page.rows
}

async fn item_row(context: &Context, id: i64) -> Row {
    item_rows(context, id)
        .await
        .into_iter()
        .next()
        .expect("item row")
}

fn update(identity: Vec<(ColumnRef, Value)>, changes: Vec<(String, Value)>) -> RowChange {
    RowChange::Update(RowUpdate { identity, changes })
}

/// Typed text converts to each column's type, NULL is stored, and the
/// returned rows are the stored values.
pub async fn row_updates_store_values_and_return_rows(context: Context) {
    let session = context.console().await;
    let items = context.describe(session.as_ref(), "items").await;
    let orders = context.describe(session.as_ref(), "orders").await;
    let weird = context.describe(session.as_ref(), WEIRD_TABLE).await;

    let updates = [
        RowEditRequest {
            details: items,
            changes: vec![update(
                vec![key("id", Value::Integer(3))],
                vec![
                    change("name", Value::Text("renamed".to_owned())),
                    change("score", Value::Decimal("2.25".to_owned())),
                    change("big", Value::Null),
                ],
            )],
        },
        RowEditRequest {
            details: orders,
            changes: vec![update(
                vec![
                    key("year", Value::Integer(2023)),
                    key("number", Value::Integer(1)),
                ],
                vec![change("note", Value::Null)],
            )],
        },
        RowEditRequest {
            details: weird,
            changes: vec![update(
                vec![key("Mixed Case", Value::Integer(1))],
                vec![change("select", Value::Text("it's \"edited\"".to_owned()))],
            )],
        },
    ];
    let mut returned = Vec::new();

    for request in updates {
        let rows = timed(session.apply_row_edits(request))
            .await
            .expect("update rows");
        returned.extend(rows.into_iter().map(|row| row.values));
    }

    close(session.as_ref()).await;

    let expected_item = vec![
        Value::Integer(3),
        Value::Text("renamed".to_owned()),
        Value::Float(2.25),
        Value::Null,
    ];
    assert_eq!(
        returned,
        vec![
            expected_item.clone(),
            vec![Value::Integer(2023), Value::Integer(1), Value::Null],
            vec![Value::Integer(1), Value::Text("it's \"edited\"".to_owned())],
        ]
    );
    assert_eq!(item_row(&context, 3).await, expected_item);
}

/// Deletes, updates, and inserts of one request run in order: a deleted
/// key can be reused, and columns an insert leaves out get their default.
pub async fn row_edits_delete_update_and_insert_in_order(context: Context) {
    let session = context.console().await;
    let items = context.describe(session.as_ref(), "items").await;
    let request = RowEditRequest {
        details: items,
        changes: vec![
            RowChange::Delete {
                identity: vec![key("id", Value::Integer(2))],
            },
            update(
                vec![key("id", Value::Integer(4))],
                vec![change("name", Value::Text("updated".to_owned()))],
            ),
            RowChange::Insert {
                values: vec![
                    change("id", Value::Integer(2)),
                    change("name", Value::Text("reinserted".to_owned())),
                ],
            },
        ],
    };

    let saved = timed(session.apply_row_edits(request))
        .await
        .expect("apply row edits");
    close(session.as_ref()).await;

    let reinserted = vec![
        Value::Integer(2),
        Value::Text("reinserted".to_owned()),
        Value::Null,
        Value::Null,
    ];
    let values: Vec<Row> = saved.into_iter().map(|row| row.values).collect();
    assert_eq!(values.len(), 3);
    assert_eq!(values[0][1], Value::Text("item 02".to_owned()));
    assert_eq!(values[1][1], Value::Text("updated".to_owned()));
    assert_eq!(values[2], reinserted);
    assert_eq!(item_row(&context, 2).await, reinserted);
    assert_eq!(
        item_row(&context, 4).await[1],
        Value::Text("updated".to_owned())
    );
}

/// A failing change rolls back the changes before it.
pub async fn row_edits_save_all_or_nothing(context: Context) {
    let session = context.console().await;
    let items = context.describe(session.as_ref(), "items").await;
    let request = RowEditRequest {
        details: items,
        changes: vec![
            RowChange::Delete {
                identity: vec![key("id", Value::Integer(5))],
            },
            update(
                vec![key("id", Value::Integer(1))],
                vec![change("name", Value::Text("lost".to_owned()))],
            ),
            update(
                vec![key("id", Value::Integer(999))],
                vec![change("name", Value::Text("lost".to_owned()))],
            ),
        ],
    };

    let error = timed(session.apply_row_edits(request))
        .await
        .expect_err("a missing row fails the save");

    assert_eq!(error.kind, ErrorKind::NotFound);
    assert_eq!(session.transaction_state(), TransactionState::Idle);
    close(session.as_ref()).await;
    assert_eq!(
        item_row(&context, 1).await[1],
        Value::Text("item 01".to_owned())
    );
    assert_eq!(item_rows(&context, 5).await.len(), 1);
}
