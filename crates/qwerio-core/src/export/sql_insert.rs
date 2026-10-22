//! One `INSERT` statement per row, with literals in the source engine's
//! syntax so the statements run against the same kind of database.

use crate::connection::EngineKind;
use crate::result::{ColumnDescriptor, Row};
use crate::value::{Value, format_float, hex_bytes};

use super::ExportTarget;
use super::json::is_number;

/// The fixed text of every statement for one target and column list.
#[derive(Clone, Debug)]
pub(super) struct InsertStatement {
    engine: EngineKind,
    /// `INSERT INTO table (columns) VALUES (`, or the whole statement when
    /// every column is generated.
    prefix: String,
    /// Positions of the columns that receive values.
    included: Vec<usize>,
}

impl InsertStatement {
    pub(super) fn new(target: &ExportTarget, columns: &[ColumnDescriptor]) -> Self {
        let engine = target.engine;
        let included: Vec<usize> = columns
            .iter()
            .enumerate()
            .filter(|(_, column)| !target.generated_columns.contains(&column.name))
            .map(|(position, _)| position)
            .collect();
        let table = table_name(target);

        let prefix = if included.is_empty() {
            all_defaults_statement(engine, &table)
        } else {
            let names: Vec<String> = included
                .iter()
                .map(|position| engine.quote_identifier(&columns[*position].name))
                .collect();
            format!("INSERT INTO {table} ({}) VALUES (", names.join(", "))
        };

        Self {
            engine,
            prefix,
            included,
        }
    }

    pub(super) fn statement(&self, row: &Row) -> String {
        if self.included.is_empty() {
            return self.prefix.clone();
        }

        let values: Vec<String> = self
            .included
            .iter()
            .map(|position| {
                row.get(*position)
                    .map_or("NULL".to_owned(), |value| literal(self.engine, value))
            })
            .collect();

        format!("{}{});", self.prefix, values.join(", "))
    }
}

/// The table's name as statements address it. PostgreSQL keeps the schema,
/// which is part of how tables are found; MySQL and SQLite statements stay
/// unqualified so they run against whichever database is current.
fn table_name(target: &ExportTarget) -> String {
    let engine = target.engine;
    let name = engine.quote_identifier(&target.object.name);

    match (engine, &target.object.namespace) {
        (EngineKind::Postgres, Some(namespace)) => {
            format!("{}.{name}", engine.quote_identifier(namespace))
        }
        _ => name,
    }
}

fn all_defaults_statement(engine: EngineKind, table: &str) -> String {
    match engine {
        EngineKind::Mysql | EngineKind::Mariadb => format!("INSERT INTO {table} () VALUES ();"),
        EngineKind::Postgres | EngineKind::Sqlite => format!("INSERT INTO {table} DEFAULT VALUES;"),
    }
}

fn literal(engine: EngineKind, value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Bool(value) => boolean(engine, *value).to_owned(),
        Value::Integer(value) => value.to_string(),
        Value::Float(value) => number_or_string(engine, &format_float(*value)),
        Value::Decimal(digits) => number_or_string(engine, digits),
        Value::Text(text) | Value::Json(text) | Value::Uuid(text) => string(engine, text),
        Value::Temporal(temporal) => string(engine, &temporal.text),
        Value::Bytes(bytes) => blob(engine, bytes),
        Value::Array(_) => string(engine, &value.canonical_text().unwrap_or_default()),
        Value::Unsupported(unsupported) => match &unsupported.text {
            Some(text) => string(engine, text),
            None => format!(
                "NULL /* unsupported {} */",
                unsupported.type_name.replace("*/", "* /")
            ),
        },
    }
}

fn boolean(engine: EngineKind, value: bool) -> &'static str {
    match (engine, value) {
        (EngineKind::Sqlite, true) => "1",
        (EngineKind::Sqlite, false) => "0",
        (_, true) => "TRUE",
        (_, false) => "FALSE",
    }
}

/// Non-finite floats and special decimals (`NaN`) become strings, which
/// PostgreSQL casts back; other engines cannot store them anyway.
fn number_or_string(engine: EngineKind, number: &str) -> String {
    if is_number(number) {
        return number.to_owned();
    }

    string(engine, number)
}

fn blob(engine: EngineKind, bytes: &[u8]) -> String {
    let hex = hex_bytes(bytes);

    match engine {
        // `'\x…'` is bytea's hex input format.
        EngineKind::Postgres => string(engine, &hex),
        EngineKind::Sqlite | EngineKind::Mysql | EngineKind::Mariadb => {
            format!("X'{}'", &hex[2..])
        }
    }
}

/// A quoted string. MySQL and MariaDB treat backslashes as escapes by
/// default, so they are escaped there too.
fn string(engine: EngineKind, text: &str) -> String {
    let escaped = match engine {
        EngineKind::Mysql | EngineKind::Mariadb => text
            .replace('\\', "\\\\")
            .replace('\0', "\\0")
            .replace('\'', "''"),
        EngineKind::Postgres | EngineKind::Sqlite => text.replace('\'', "''"),
    };

    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_escape_quotes_and_mysql_backslashes() {
        let text = "it's C:\\temp";

        assert_eq!(string(EngineKind::Postgres, text), "'it''s C:\\temp'");
        assert_eq!(string(EngineKind::Mysql, text), "'it''s C:\\\\temp'");
    }

    #[test]
    fn literals_follow_the_engine() {
        assert_eq!(literal(EngineKind::Sqlite, &Value::Bool(true)), "1");
        assert_eq!(literal(EngineKind::Postgres, &Value::Bool(false)), "FALSE");
        assert_eq!(
            literal(EngineKind::Postgres, &Value::Bytes(vec![0xab, 0x01])),
            "'\\xab01'"
        );
        assert_eq!(
            literal(EngineKind::Mariadb, &Value::Bytes(vec![0xab])),
            "X'ab'"
        );
        assert_eq!(
            literal(EngineKind::Postgres, &Value::Float(f64::NAN)),
            "'NaN'"
        );
        assert_eq!(
            literal(EngineKind::Postgres, &Value::Decimal("12.50".to_owned())),
            "12.50"
        );
    }
}
