//! Relationship navigation from declared foreign keys, including composite keys.

use crate::catalog::{ForeignKeyInfo, ObjectDetails};
use crate::filter::Filter;
use crate::resource::ObjectRef;
use crate::result::Row;
use crate::value::Value;

/// Where following a relationship leads: an object and the filters that select
/// the related rows.
#[derive(Clone, Debug, PartialEq)]
pub struct RelationshipTarget {
    pub object: ObjectRef,
    pub filters: Vec<Filter>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RelationshipOutcome {
    Target(RelationshipTarget),
    /// A key column is NULL, so no related row exists (MATCH SIMPLE).
    NullKey,
    /// The row does not contain the key columns, for example because the
    /// object's metadata changed.
    MissingColumns(Vec<String>),
}

/// Follows a foreign key declared on `details.object` from one of its rows to
/// the referenced row.
pub fn follow_referenced(
    details: &ObjectDetails,
    row: &Row,
    foreign_key: &ForeignKeyInfo,
) -> RelationshipOutcome {
    build_target(
        details,
        row,
        &foreign_key.columns,
        &foreign_key.referenced_columns,
        &foreign_key.referenced_table,
    )
}

/// Follows a foreign key on another object that references `details.object`,
/// selecting the rows that point at this row.
pub fn follow_referencing(
    details: &ObjectDetails,
    row: &Row,
    foreign_key: &ForeignKeyInfo,
) -> RelationshipOutcome {
    build_target(
        details,
        row,
        &foreign_key.referenced_columns,
        &foreign_key.columns,
        &foreign_key.table,
    )
}

fn build_target(
    details: &ObjectDetails,
    row: &Row,
    source_columns: &[String],
    target_columns: &[String],
    target: &ObjectRef,
) -> RelationshipOutcome {
    let mut values: Vec<&Value> = Vec::with_capacity(source_columns.len());
    let mut missing = Vec::new();

    for column in source_columns {
        let position = details.columns.iter().position(|info| &info.name == column);

        match position.and_then(|index| row.get(index)) {
            Some(value) => values.push(value),
            None => missing.push(column.clone()),
        }
    }

    if !missing.is_empty() {
        return RelationshipOutcome::MissingColumns(missing);
    }

    if values.iter().any(|value| value.is_null()) {
        return RelationshipOutcome::NullKey;
    }

    let filters = target_columns
        .iter()
        .zip(values)
        .map(|(column, value)| Filter::equals(column.clone(), value.clone()))
        .collect();

    RelationshipOutcome::Target(RelationshipTarget {
        object: target.clone(),
        filters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ColumnInfo;
    use crate::ids::ConnectionId;
    use crate::resource::ObjectKind;

    fn object(connection: ConnectionId, name: &str) -> ObjectRef {
        ObjectRef {
            connection,
            database: None,
            namespace: Some("public".to_owned()),
            kind: ObjectKind::Table,
            name: name.to_owned(),
        }
    }

    fn details_with_columns(object: ObjectRef, names: &[&str]) -> ObjectDetails {
        ObjectDetails {
            object,
            columns: names
                .iter()
                .enumerate()
                .map(|(index, name)| ColumnInfo {
                    name: (*name).to_owned(),
                    type_name: "integer".to_owned(),
                    cast_type: None,
                    nullable: true,
                    default: None,
                    ordinal: index + 1,
                    decodable: true,
                    generated: false,
                    comment: None,
                })
                .collect(),
            primary_key: None,
            unique_keys: Vec::new(),
            row_locator: None,
            foreign_keys: Vec::new(),
            referencing_keys: Vec::new(),
            comment: None,
        }
    }

    fn composite_key(connection: ConnectionId) -> ForeignKeyInfo {
        ForeignKeyInfo {
            name: Some("line_order".to_owned()),
            table: object(connection, "lines"),
            // Declared order differs from the table's column order.
            columns: vec!["order_year".to_owned(), "order_no".to_owned()],
            referenced_table: object(connection, "orders"),
            referenced_columns: vec!["year".to_owned(), "number".to_owned()],
        }
    }

    #[test]
    fn composite_keys_follow_declared_column_order() {
        let connection = ConnectionId::new();
        let lines = details_with_columns(
            object(connection, "lines"),
            &["id", "order_no", "order_year"],
        );
        let row = vec![Value::Integer(1), Value::Integer(42), Value::Integer(2024)];

        let outcome = follow_referenced(&lines, &row, &composite_key(connection));

        assert_eq!(
            outcome,
            RelationshipOutcome::Target(RelationshipTarget {
                object: object(connection, "orders"),
                filters: vec![
                    Filter::equals("year", Value::Integer(2024)),
                    Filter::equals("number", Value::Integer(42)),
                ],
            })
        );
    }

    #[test]
    fn referencing_rows_are_selected_from_the_other_table() {
        let connection = ConnectionId::new();
        let orders = details_with_columns(object(connection, "orders"), &["number", "year"]);
        let row = vec![Value::Integer(42), Value::Integer(2024)];

        let outcome = follow_referencing(&orders, &row, &composite_key(connection));

        assert_eq!(
            outcome,
            RelationshipOutcome::Target(RelationshipTarget {
                object: object(connection, "lines"),
                filters: vec![
                    Filter::equals("order_year", Value::Integer(2024)),
                    Filter::equals("order_no", Value::Integer(42)),
                ],
            })
        );
    }

    #[test]
    fn null_key_parts_mean_no_related_row() {
        let connection = ConnectionId::new();
        let lines = details_with_columns(
            object(connection, "lines"),
            &["id", "order_no", "order_year"],
        );
        let row = vec![Value::Integer(1), Value::Integer(42), Value::Null];

        assert_eq!(
            follow_referenced(&lines, &row, &composite_key(connection)),
            RelationshipOutcome::NullKey
        );
    }

    #[test]
    fn missing_key_columns_are_reported() {
        let connection = ConnectionId::new();
        let lines = details_with_columns(object(connection, "lines"), &["id", "order_no"]);
        let row = vec![Value::Integer(1), Value::Integer(42)];

        assert_eq!(
            follow_referenced(&lines, &row, &composite_key(connection)),
            RelationshipOutcome::MissingColumns(vec!["order_year".to_owned()])
        );
    }
}
