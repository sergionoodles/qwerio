//! PostgreSQL SQL syntax for generated browsing queries. Every parameter is
//! bound as text and cast to the compared column's type, so user-entered
//! filter text and ordering keys convert through PostgreSQL's input functions.

use qwerio_core::catalog::ColumnInfo;
use qwerio_core::resource::ObjectRef;
use qwerio_core::sql::{SqlDialect, quote_identifier_ansi};

pub(crate) struct PostgresDialect;

impl SqlDialect for PostgresDialect {
    fn quote_identifier(&self, identifier: &str) -> String {
        quote_identifier_ansi(identifier)
    }

    fn qualified_name(&self, object: &ObjectRef) -> String {
        match &object.namespace {
            Some(namespace) => format!(
                "{}.{}",
                quote_identifier_ansi(namespace),
                quote_identifier_ansi(&object.name)
            ),
            None => quote_identifier_ansi(&object.name),
        }
    }

    fn placeholder(&self, index: usize, column: Option<&ColumnInfo>) -> String {
        match column.map(cast_type) {
            Some(type_name) => format!("CAST(${index} AS {type_name})"),
            None => format!("${index}"),
        }
    }

    fn case_insensitive_like(&self, expression: &str, pattern_placeholder: &str) -> String {
        format!("CAST({expression} AS text) ILIKE {pattern_placeholder} ESCAPE '\\'")
    }

    /// Text output round-trips exactly through the column type's input
    /// function, including floating point (PostgreSQL 12+ prints shortest
    /// exact values).
    fn key_expression(&self, expression: &str, _column: Option<&ColumnInfo>) -> String {
        format!("CAST({expression} AS text)")
    }

    fn select_expression(&self, expression: &str, column: &ColumnInfo) -> String {
        if column.decodable {
            return expression.to_owned();
        }

        format!(
            "CAST({expression} AS text) AS {}",
            quote_identifier_ansi(&column.name)
        )
    }
}

/// The type to cast parameters to: `format_type` output without modifiers,
/// which PostgreSQL already quotes and schema-qualifies as needed.
fn cast_type(column: &ColumnInfo) -> &str {
    column.cast_type.as_deref().unwrap_or(&column.type_name)
}
