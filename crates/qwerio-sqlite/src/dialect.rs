//! SQLite SQL syntax for generated browsing queries.

use qwerio_core::catalog::ColumnInfo;
use qwerio_core::resource::ObjectRef;
use qwerio_core::sql::{SqlDialect, quote_identifier_ansi};

/// The only schema a session browses. Attached databases are out of scope.
pub(crate) const MAIN_SCHEMA: &str = "main";

pub(crate) struct SqliteDialect;

impl SqlDialect for SqliteDialect {
    fn quote_identifier(&self, identifier: &str) -> String {
        quote_identifier_ansi(identifier)
    }

    fn qualified_name(&self, object: &ObjectRef) -> String {
        format!(
            "{}.{}",
            quote_identifier_ansi(MAIN_SCHEMA),
            quote_identifier_ansi(&object.name)
        )
    }

    fn placeholder(&self, index: usize, _column: Option<&ColumnInfo>) -> String {
        format!("?{index}")
    }

    /// SQLite's LIKE folds ASCII letters only.
    fn case_insensitive_like(&self, expression: &str, pattern_placeholder: &str) -> String {
        format!("CAST({expression} AS TEXT) LIKE {pattern_placeholder} ESCAPE '\\'")
    }

    /// Keys are bound back with their storage class, so the raw value is exact.
    fn key_expression(&self, expression: &str, _column: Option<&ColumnInfo>) -> String {
        expression.to_owned()
    }
}
