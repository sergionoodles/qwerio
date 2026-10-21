//! MySQL and MariaDB syntax for generated queries.

use qwerio_core::catalog::ColumnInfo;
use qwerio_core::connection::EngineKind;
use qwerio_core::filter::{NullsOrder, SortDirection};
use qwerio_core::resource::ObjectRef;
use qwerio_core::sql::SqlDialect;

pub(crate) struct MysqlDialect;

impl SqlDialect for MysqlDialect {
    fn quote_identifier(&self, identifier: &str) -> String {
        EngineKind::Mysql.quote_identifier(identifier)
    }

    fn qualified_name(&self, object: &ObjectRef) -> String {
        let name = self.quote_identifier(&object.name);
        match &object.database {
            Some(database) => format!("{}.{name}", self.quote_identifier(database)),
            None => name,
        }
    }

    fn placeholder(&self, _index: usize, _column: Option<&ColumnInfo>) -> String {
        "?".to_owned()
    }

    fn case_insensitive_like(&self, expression: &str, pattern_placeholder: &str) -> String {
        // Hex avoids dependence on NO_BACKSLASH_ESCAPES. An explicit collation
        // keeps case folding independent of the column/server defaults.
        format!(
            "CONVERT({expression} USING utf8mb4) COLLATE utf8mb4_unicode_ci LIKE {pattern_placeholder} ESCAPE 0x5c"
        )
    }

    fn order_expression(
        &self,
        expression: &str,
        direction: SortDirection,
        nulls: NullsOrder,
    ) -> String {
        let direction = match direction {
            SortDirection::Ascending => "ASC",
            SortDirection::Descending => "DESC",
        };
        let null_direction = match nulls {
            NullsOrder::First => "DESC",
            NullsOrder::Last => "ASC",
        };
        format!("({expression} IS NULL) {null_direction}, {expression} {direction}")
    }

    fn key_expression(&self, expression: &str, _column: Option<&ColumnInfo>) -> String {
        expression.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_supports_both_null_positions() {
        assert_eq!(
            MysqlDialect.order_expression("`a`", SortDirection::Ascending, NullsOrder::Last),
            "(`a` IS NULL) ASC, `a` ASC"
        );
        assert_eq!(
            MysqlDialect.order_expression("`a`", SortDirection::Descending, NullsOrder::First),
            "(`a` IS NULL) DESC, `a` DESC"
        );
    }

    #[test]
    fn identifiers_escape_backticks() {
        assert_eq!(MysqlDialect.quote_identifier("a`b"), "`a``b`");
    }
}
