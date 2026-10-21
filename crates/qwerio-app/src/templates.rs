//! Starter text for documents created beside database objects. The text is
//! only inserted into an editor; nothing runs until the user runs it.

use qwerio_core::connection::EngineKind;
use qwerio_core::resource::ObjectRef;

/// Rows a starter query asks for.
pub const STARTER_ROW_LIMIT: usize = 100;

/// `SELECT * FROM <object> LIMIT 100;` with every identifier quoted. Both
/// PostgreSQL and SQLite accept this form.
pub fn select_statement(object: &ObjectRef) -> String {
    select_statement_for_engine(object, EngineKind::Postgres)
}

pub fn select_statement_for_engine(object: &ObjectRef, engine: EngineKind) -> String {
    let name = match &object.namespace {
        Some(namespace) => format!(
            "{}.{}",
            engine.quote_identifier(namespace),
            engine.quote_identifier(&object.name)
        ),
        None => engine.quote_identifier(&object.name),
    };

    format!("SELECT *\nFROM {name}\nLIMIT {STARTER_ROW_LIMIT};\n")
}

pub fn note(object: &ObjectRef) -> String {
    format!("# {}\n\n", object.qualified_name())
}

/// A file-name stem derived from an object name: letters, digits, `-`, and
/// `_`, never empty.
pub fn file_stem(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = stem.trim_matches('-');

    if trimmed.is_empty() {
        "untitled".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use qwerio_core::ids::ConnectionId;
    use qwerio_core::resource::ObjectKind;

    use super::*;

    fn object(namespace: Option<&str>, name: &str) -> ObjectRef {
        ObjectRef {
            connection: ConnectionId::new(),
            database: None,
            namespace: namespace.map(str::to_owned),
            kind: ObjectKind::Table,
            name: name.to_owned(),
        }
    }

    #[test]
    fn mysql_starter_queries_quote_names() {
        for engine in [EngineKind::Mysql, EngineKind::Mariadb] {
            assert_eq!(
                select_statement_for_engine(&object(None, "Order`Lines"), engine),
                "SELECT *\nFROM `Order``Lines`\nLIMIT 100;\n"
            );
        }
    }

    #[test]
    fn starter_queries_quote_names() {
        assert_eq!(
            select_statement(&object(Some("sales"), "Order \"Lines\"")),
            "SELECT *\nFROM \"sales\".\"Order \"\"Lines\"\"\"\nLIMIT 100;\n"
        );
        assert_eq!(
            select_statement(&object(None, "items")),
            "SELECT *\nFROM \"items\"\nLIMIT 100;\n"
        );
    }

    #[test]
    fn file_stems_are_safe_and_never_empty() {
        assert_eq!(file_stem("order lines/2024"), "order-lines-2024");
        assert_eq!(file_stem("../.."), "untitled");
        assert_eq!(file_stem("Kunden_ä"), "Kunden_ä");
    }
}
