//! Row changes staged in the table browser: updated cells, new rows, and
//! deleted rows. Shared code decides which rows can be edited and renders one
//! `INSERT`, `UPDATE`, or `DELETE ... RETURNING` per row; adapters run them in
//! one transaction and require each to touch exactly one row.

use std::sync::Arc;

use crate::catalog::{ColumnInfo, ObjectDetails};
use crate::error::{DatabaseError, DbResult, ErrorKind};
use crate::resource::ObjectKind;
use crate::result::Row;
use crate::sql::{
    ColumnRef, CompareOp, OBJECT_ALIAS, Predicate, RenderedSql, SqlDialect, SqlRenderer,
    row_identity,
};
use crate::value::Value;

/// The values that find one row: [`crate::sql::row_identity`] columns as
/// last read.
pub type RowIdentity = Vec<(ColumnRef, Value)>;

/// New values for some columns of one row, which is found by its identity.
#[derive(Clone, Debug, PartialEq)]
pub struct RowUpdate {
    pub identity: RowIdentity,
    /// Column names and their new values.
    pub changes: Vec<(String, Value)>,
}

/// One change to a table's rows.
#[derive(Clone, Debug, PartialEq)]
pub enum RowChange {
    /// A new row with these column values; other columns get their default.
    Insert {
        values: Vec<(String, Value)>,
    },
    Update(RowUpdate),
    Delete {
        identity: RowIdentity,
    },
}

/// Row changes applied together, in order: all of them or none.
#[derive(Clone, Debug)]
pub struct RowEditRequest {
    pub details: Arc<ObjectDetails>,
    pub changes: Vec<RowChange>,
}

/// A row as stored after a change, or as it was before being deleted.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedRow {
    /// The data columns, like a browsed page row.
    pub values: Row,
    /// The hidden row locator, when rows are identified by it.
    pub locator: Option<Value>,
}

/// Why the rows of an object cannot be edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotEditable {
    /// Only tables are edited; views and other objects are read-only.
    NotATable,
    /// No unique key or row locator identifies a single row.
    NoRowIdentity,
}

impl NotEditable {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotATable => "Only tables can be edited",
            Self::NoRowIdentity => "Rows cannot be edited: the table has no primary or unique key",
        }
    }
}

/// Checks whether the rows of an object can be edited inline.
pub fn check_editable(details: &ObjectDetails) -> Result<(), NotEditable> {
    if details.object.kind != ObjectKind::Table {
        return Err(NotEditable::NotATable);
    }

    if row_identity(details).is_none() {
        return Err(NotEditable::NoRowIdentity);
    }

    Ok(())
}

/// Whether a new row copied from an existing one takes this column's value.
/// Key columns must be unique, so copying them would always conflict, and
/// generated columns cannot be written; both are left to the database.
pub fn is_copied_to_new_rows(details: &ObjectDetails, column: &ColumnInfo) -> bool {
    let is_key_column = details
        .primary_key
        .iter()
        .chain(&details.unique_keys)
        .any(|key| key.columns.contains(&column.name));

    !is_key_column && !column.generated
}

/// Renders the statement for one change. It returns the touched row with the
/// columns of a browsed page row, followed by the row locator when rows are
/// identified by it; see [`saved_row`].
pub fn render_change(
    dialect: &dyn SqlDialect,
    details: &ObjectDetails,
    change: &RowChange,
) -> DbResult<RenderedSql> {
    check_editable(details)
        .map_err(|reason| DatabaseError::new(ErrorKind::Unsupported, reason.message()))?;

    match change {
        RowChange::Insert { values } => render_insert(dialect, details, values),
        RowChange::Update(update) => render_update(dialect, details, update),
        RowChange::Delete { identity } => render_delete(dialect, details, identity),
    }
}

/// `INSERT INTO object (...) VALUES (...)`, or `DEFAULT VALUES` when no
/// column is set.
fn render_insert(
    dialect: &dyn SqlDialect,
    details: &ObjectDetails,
    values: &[(String, Value)],
) -> DbResult<RenderedSql> {
    let mut renderer = SqlRenderer::new(dialect, details);
    let mut names = Vec::with_capacity(values.len());
    let mut placeholders = Vec::with_capacity(values.len());

    for (name, value) in values {
        let column = writable_column(details, name)?;
        names.push(dialect.quote_identifier(name));
        placeholders.push(renderer.bind(value.clone(), Some(column)));
    }

    let source = if names.is_empty() {
        "DEFAULT VALUES".to_owned()
    } else {
        format!(
            "({}) VALUES ({})",
            names.join(", "),
            placeholders.join(", ")
        )
    };

    let text = format!(
        "INSERT INTO {} {source} RETURNING {}",
        dialect.qualified_name(&details.object),
        returning(dialect, details),
    );

    Ok(RenderedSql {
        text,
        params: renderer.into_params(),
    })
}

/// `UPDATE object AS alias SET ... WHERE identity`.
fn render_update(
    dialect: &dyn SqlDialect,
    details: &ObjectDetails,
    update: &RowUpdate,
) -> DbResult<RenderedSql> {
    if update.changes.is_empty() {
        return Err(DatabaseError::invalid_request(
            "The update changes no column",
        ));
    }

    let mut renderer = SqlRenderer::new(dialect, details);

    let assignments = update
        .changes
        .iter()
        .map(|(name, value)| {
            let column = writable_column(details, name)?;
            let placeholder = renderer.bind(value.clone(), Some(column));
            Ok(format!(
                "{} = {placeholder}",
                dialect.quote_identifier(name)
            ))
        })
        .collect::<DbResult<Vec<_>>>()?;

    let where_clause = render_identity(&mut renderer, &update.identity)?;

    let text = format!(
        "UPDATE {} AS {} SET {} WHERE {where_clause} RETURNING {}",
        dialect.qualified_name(&details.object),
        dialect.quote_identifier(OBJECT_ALIAS),
        assignments.join(", "),
        returning(dialect, details),
    );

    Ok(RenderedSql {
        text,
        params: renderer.into_params(),
    })
}

/// `DELETE FROM object AS alias WHERE identity`.
fn render_delete(
    dialect: &dyn SqlDialect,
    details: &ObjectDetails,
    identity: &[(ColumnRef, Value)],
) -> DbResult<RenderedSql> {
    let mut renderer = SqlRenderer::new(dialect, details);
    let where_clause = render_identity(&mut renderer, identity)?;

    let text = format!(
        "DELETE FROM {} AS {} WHERE {where_clause} RETURNING {}",
        dialect.qualified_name(&details.object),
        dialect.quote_identifier(OBJECT_ALIAS),
        returning(dialect, details),
    );

    Ok(RenderedSql {
        text,
        params: renderer.into_params(),
    })
}

/// A column that changes may set.
fn writable_column<'a>(details: &'a ObjectDetails, name: &str) -> DbResult<&'a ColumnInfo> {
    let column = details
        .column(name)
        .ok_or_else(|| DatabaseError::invalid_request(format!("Unknown column \"{name}\"")))?;

    if column.generated {
        return Err(DatabaseError::invalid_request(format!(
            "Column \"{name}\" is generated by the database"
        )));
    }

    Ok(column)
}

fn render_identity(
    renderer: &mut SqlRenderer<'_>,
    identity: &[(ColumnRef, Value)],
) -> DbResult<String> {
    if identity.is_empty() || identity.iter().any(|(_, value)| value.is_null()) {
        return Err(DatabaseError::invalid_request(
            "The row is not identified by NULL-free key values",
        ));
    }

    let predicate = Predicate::and(
        identity
            .iter()
            .map(|(column, value)| Predicate::Compare {
                column: column.clone(),
                op: CompareOp::Equal,
                value: value.clone(),
            })
            .collect(),
    );

    renderer.render_predicate(&predicate)
}

/// The data columns, then the locator when rows are identified by it.
/// Unqualified: the target table is the only one in scope.
fn returning(dialect: &dyn SqlDialect, details: &ObjectDetails) -> String {
    let columns = details
        .columns
        .iter()
        .map(|column| dialect.select_expression(&dialect.quote_identifier(&column.name), column));

    let locator = details
        .row_locator
        .as_ref()
        .filter(|_| identifies_by_locator(details))
        .map(|locator| locator.expression.clone());

    columns.chain(locator).collect::<Vec<_>>().join(", ")
}

fn identifies_by_locator(details: &ObjectDetails) -> bool {
    row_identity(details).is_some_and(|identity| identity == [ColumnRef::Locator])
}

/// The number of columns a change statement returns.
pub fn returned_column_count(details: &ObjectDetails) -> usize {
    details.columns.len() + usize::from(identifies_by_locator(details))
}

/// Checks that a change touched exactly one row and returns that row.
pub fn saved_row(details: &ObjectDetails, mut rows: Vec<Row>) -> DbResult<SavedRow> {
    let mut values = match rows.len() {
        1 => rows.remove(0),
        0 => {
            return Err(DatabaseError::new(
                ErrorKind::NotFound,
                "A row was changed or deleted since it was loaded; refresh and try again",
            ));
        }
        count => {
            return Err(DatabaseError::new(
                ErrorKind::Internal,
                format!("A change matched {count} rows instead of one; nothing was saved"),
            ));
        }
    };

    let locator = identifies_by_locator(details)
        .then(|| values.pop())
        .flatten();

    Ok(SavedRow { values, locator })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{KeyInfo, RowLocator};
    use crate::ids::ConnectionId;
    use crate::resource::ObjectRef;
    use crate::sql::quote_identifier_ansi;

    struct TestDialect;

    impl SqlDialect for TestDialect {
        fn quote_identifier(&self, identifier: &str) -> String {
            quote_identifier_ansi(identifier)
        }

        fn qualified_name(&self, object: &ObjectRef) -> String {
            quote_identifier_ansi(&object.name)
        }

        fn placeholder(&self, index: usize, column: Option<&ColumnInfo>) -> String {
            match column {
                Some(column) => format!("CAST(${index} AS {})", column.type_name),
                None => format!("${index}"),
            }
        }

        fn case_insensitive_like(&self, expression: &str, pattern_placeholder: &str) -> String {
            format!("{expression} LIKE {pattern_placeholder}")
        }

        fn key_expression(&self, expression: &str, _column: Option<&ColumnInfo>) -> String {
            expression.to_owned()
        }
    }

    fn column(name: &str, type_name: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            cast_type: None,
            nullable: true,
            default: None,
            ordinal: 1,
            comment: None,
            decodable: true,
            generated: false,
        }
    }

    fn details(kind: ObjectKind, primary_key: bool, locator: bool) -> ObjectDetails {
        ObjectDetails {
            object: ObjectRef {
                connection: ConnectionId::from_uuid(uuid::Uuid::nil()),
                database: None,
                namespace: None,
                kind,
                name: "items".to_owned(),
            },
            columns: vec![column("id", "integer"), column("name", "text")],
            primary_key: primary_key.then(|| KeyInfo {
                name: None,
                columns: vec!["id".to_owned()],
                allows_null: false,
            }),
            unique_keys: Vec::new(),
            row_locator: locator.then(|| RowLocator {
                expression: "rowid".to_owned(),
                type_name: "integer".to_owned(),
            }),
            foreign_keys: Vec::new(),
            referencing_keys: Vec::new(),
            comment: None,
        }
    }

    fn by_id(id: i64) -> RowIdentity {
        vec![(ColumnRef::Column("id".to_owned()), Value::Integer(id))]
    }

    fn rename(identity: RowIdentity) -> RowChange {
        RowChange::Update(RowUpdate {
            identity,
            changes: vec![("name".to_owned(), Value::Text("Pen".to_owned()))],
        })
    }

    fn render(details: &ObjectDetails, change: &RowChange) -> DbResult<RenderedSql> {
        render_change(&TestDialect, details, change)
    }

    #[test]
    fn updates_set_columns_by_key_and_return_the_row() {
        let details = details(ObjectKind::Table, true, false);

        let sql = render(&details, &rename(by_id(7))).expect("render");

        assert_eq!(
            sql.text,
            "UPDATE \"items\" AS \"qwerio_object\" SET \"name\" = CAST($1 AS text) \
             WHERE \"qwerio_object\".\"id\" = CAST($2 AS integer) RETURNING \"id\", \"name\""
        );
        assert_eq!(
            sql.params,
            vec![Value::Text("Pen".to_owned()), Value::Integer(7)]
        );
    }

    #[test]
    fn inserts_set_given_columns_and_leave_the_rest_to_defaults() {
        let details = details(ObjectKind::Table, true, false);
        let insert = RowChange::Insert {
            values: vec![("name".to_owned(), Value::Text("Pen".to_owned()))],
        };
        let empty = RowChange::Insert { values: Vec::new() };

        let sql = render(&details, &insert).expect("render");
        let defaults = render(&details, &empty).expect("render");

        assert_eq!(
            sql.text,
            "INSERT INTO \"items\" (\"name\") VALUES (CAST($1 AS text)) RETURNING \"id\", \"name\""
        );
        assert_eq!(sql.params, vec![Value::Text("Pen".to_owned())]);
        assert_eq!(
            defaults.text,
            "INSERT INTO \"items\" DEFAULT VALUES RETURNING \"id\", \"name\""
        );
    }

    #[test]
    fn deletes_find_the_row_by_key_and_return_it() {
        let details = details(ObjectKind::Table, true, false);
        let delete = RowChange::Delete { identity: by_id(7) };

        let sql = render(&details, &delete).expect("render");

        assert_eq!(
            sql.text,
            "DELETE FROM \"items\" AS \"qwerio_object\" \
             WHERE \"qwerio_object\".\"id\" = CAST($1 AS integer) RETURNING \"id\", \"name\""
        );
    }

    #[test]
    fn rows_without_a_key_are_found_by_their_locator_and_return_it() {
        let details = details(ObjectKind::Table, false, true);
        let update = rename(vec![(ColumnRef::Locator, Value::Integer(3))]);

        let sql = render(&details, &update).expect("render");
        let saved = saved_row(
            &details,
            vec![vec![Value::Integer(1), Value::Null, Value::Integer(3)]],
        );

        assert!(sql.text.contains("WHERE \"qwerio_object\".rowid = $2"));
        assert!(sql.text.ends_with("RETURNING \"id\", \"name\", rowid"));
        assert_eq!(returned_column_count(&details), 3);
        assert_eq!(
            saved,
            Ok(SavedRow {
                values: vec![Value::Integer(1), Value::Null],
                locator: Some(Value::Integer(3)),
            })
        );
    }

    #[test]
    fn views_and_keyless_tables_are_not_editable() {
        assert_eq!(
            check_editable(&details(ObjectKind::View, true, false)),
            Err(NotEditable::NotATable)
        );
        assert_eq!(
            check_editable(&details(ObjectKind::Table, false, false)),
            Err(NotEditable::NoRowIdentity)
        );
        assert_eq!(
            check_editable(&details(ObjectKind::Table, true, false)),
            Ok(())
        );
    }

    #[test]
    fn changes_without_identity_or_to_unknown_or_generated_columns_are_rejected() {
        let mut details = details(ObjectKind::Table, true, false);
        details.columns[1].generated = true;
        let unknown_column = RowChange::Update(RowUpdate {
            identity: by_id(1),
            changes: vec![("missing".to_owned(), Value::Null)],
        });
        let generated = RowChange::Insert {
            values: vec![("name".to_owned(), Value::Null)],
        };
        let null_key = RowChange::Delete {
            identity: vec![(ColumnRef::Column("id".to_owned()), Value::Null)],
        };

        assert!(render(&details, &rename(Vec::new())).is_err());
        assert!(render(&details, &unknown_column).is_err());
        assert!(render(&details, &generated).is_err());
        assert!(render(&details, &null_key).is_err());
    }

    #[test]
    fn copies_leave_out_key_and_generated_columns() {
        let mut details = details(ObjectKind::Table, true, false);
        details.columns.push(ColumnInfo {
            generated: true,
            ..column("total", "integer")
        });

        let copied: Vec<&str> = details
            .columns
            .iter()
            .filter(|column| is_copied_to_new_rows(&details, column))
            .map(|column| column.name.as_str())
            .collect();

        assert_eq!(copied, vec!["name"]);
    }

    #[test]
    fn exactly_one_changed_row_is_required() {
        let details = details(ObjectKind::Table, true, false);
        let row = vec![Value::Integer(1)];

        assert_eq!(
            saved_row(&details, vec![row.clone()]),
            Ok(SavedRow {
                values: row.clone(),
                locator: None,
            })
        );
        assert_eq!(
            saved_row(&details, Vec::new()).map_err(|error| error.kind),
            Err(ErrorKind::NotFound)
        );
        assert!(saved_row(&details, vec![row.clone(), row]).is_err());
    }
}
