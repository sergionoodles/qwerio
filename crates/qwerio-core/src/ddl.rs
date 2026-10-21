//! Schema changes made from forms: a new database, table, or column.
//!
//! Names are always quoted, so any name is used verbatim. Column types and
//! default expressions are SQL fragments typed by the user; the rendered
//! statement is shown before it runs, and adapters reject anything that is
//! not exactly one statement.

use crate::connection::EngineKind;
use crate::error::{DatabaseError, DbResult};
use crate::resource::{CatalogPath, ObjectKind, ObjectRef};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnDefinition {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    /// A SQL expression, used verbatim.
    pub default: Option<String>,
    pub is_primary_key: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaChange {
    CreateDatabase {
        name: String,
    },
    CreateTable {
        container: CatalogPath,
        name: String,
        columns: Vec<ColumnDefinition>,
    },
    AddColumn {
        table: ObjectRef,
        column: ColumnDefinition,
    },
}

impl SchemaChange {
    /// The single statement that applies the change, or why it is incomplete.
    pub fn render(&self) -> DbResult<String> {
        self.render_for_engine(EngineKind::Postgres)
    }

    pub fn render_for_engine(&self, engine: EngineKind) -> DbResult<String> {
        match self {
            Self::CreateDatabase { name } => {
                require_name(name, "Enter a name for the database")?;
                Ok(format!("CREATE DATABASE {}", engine.quote_identifier(name)))
            }
            Self::CreateTable {
                container,
                name,
                columns,
            } => render_create_table(engine, container, name, columns),
            Self::AddColumn { table, column } => render_add_column(engine, table, column),
        }
    }
}

fn render_create_table(
    engine: EngineKind,
    container: &CatalogPath,
    name: &str,
    columns: &[ColumnDefinition],
) -> DbResult<String> {
    require_name(name, "Enter a name for the table")?;

    if columns.is_empty() {
        return Err(DatabaseError::invalid_request("Add at least one column"));
    }

    for (index, column) in columns.iter().enumerate() {
        validate_column(column)?;

        if columns[..index]
            .iter()
            .any(|earlier| earlier.name == column.name)
        {
            return Err(DatabaseError::invalid_request(format!(
                "There are two columns named \"{}\"",
                column.name
            )));
        }
    }

    let key_columns: Vec<&ColumnDefinition> = columns
        .iter()
        .filter(|column| column.is_primary_key)
        .collect();
    // One key column is declared inline, which keeps SQLite's INTEGER
    // PRIMARY KEY a rowid alias; several need a table constraint.
    let has_inline_key = key_columns.len() == 1;

    let mut lines: Vec<String> = columns
        .iter()
        .map(|column| column_sql(engine, column, has_inline_key && column.is_primary_key))
        .collect();

    if key_columns.len() > 1 {
        let names: Vec<String> = key_columns
            .iter()
            .map(|column| engine.quote_identifier(&column.name))
            .collect();
        lines.push(format!("PRIMARY KEY ({})", names.join(", ")));
    }

    Ok(format!(
        "CREATE TABLE {} (\n    {}\n)",
        qualified_name(engine, container.namespace.as_deref(), name),
        lines.join(",\n    ")
    ))
}

fn render_add_column(
    engine: EngineKind,
    table: &ObjectRef,
    column: &ColumnDefinition,
) -> DbResult<String> {
    if table.kind != ObjectKind::Table {
        return Err(DatabaseError::invalid_request(format!(
            "Columns can only be added to tables, and \"{}\" is a {}",
            table.name,
            table.kind.display_name().to_lowercase()
        )));
    }

    if column.is_primary_key {
        return Err(DatabaseError::invalid_request(
            "Set the primary key when creating the table",
        ));
    }

    validate_column(column)?;

    Ok(format!(
        "ALTER TABLE {} ADD COLUMN {}",
        qualified_name(engine, table.namespace.as_deref(), &table.name),
        column_sql(engine, column, false)
    ))
}

fn validate_column(column: &ColumnDefinition) -> DbResult<()> {
    require_name(&column.name, "Enter a name for every column")?;

    if column.data_type.trim().is_empty() {
        return Err(DatabaseError::invalid_request(format!(
            "Choose a type for \"{}\"",
            column.name
        )));
    }

    Ok(())
}

fn require_name(name: &str, message: &str) -> DbResult<()> {
    if name.trim().is_empty() {
        return Err(DatabaseError::invalid_request(message));
    }

    Ok(())
}

fn column_sql(engine: EngineKind, column: &ColumnDefinition, is_inline_key: bool) -> String {
    let mut sql = format!(
        "{} {}",
        engine.quote_identifier(&column.name),
        column.data_type.trim()
    );

    if !column.is_nullable {
        sql.push_str(" NOT NULL");
    }

    if let Some(default) = column.default.as_deref().map(str::trim)
        && !default.is_empty()
    {
        sql.push_str(" DEFAULT ");
        sql.push_str(default);
    }

    if is_inline_key {
        sql.push_str(" PRIMARY KEY");
    }

    sql
}

fn qualified_name(engine: EngineKind, namespace: Option<&str>, name: &str) -> String {
    match namespace {
        Some(namespace) => format!(
            "{}.{}",
            engine.quote_identifier(namespace),
            engine.quote_identifier(name)
        ),
        None => engine.quote_identifier(name),
    }
}

#[cfg(test)]
mod tests {
    use crate::error::ErrorKind;
    use crate::ids::ConnectionId;

    use super::*;

    fn column(name: &str, data_type: &str) -> ColumnDefinition {
        ColumnDefinition {
            name: name.to_owned(),
            data_type: data_type.to_owned(),
            is_nullable: true,
            default: None,
            is_primary_key: false,
        }
    }

    fn key(name: &str, data_type: &str) -> ColumnDefinition {
        ColumnDefinition {
            is_nullable: false,
            is_primary_key: true,
            ..column(name, data_type)
        }
    }

    fn table(namespace: Option<&str>, name: &str, kind: ObjectKind) -> ObjectRef {
        ObjectRef {
            connection: ConnectionId::new(),
            database: Some("app".to_owned()),
            namespace: namespace.map(str::to_owned),
            kind,
            name: name.to_owned(),
        }
    }

    fn create_table(namespace: Option<&str>, columns: Vec<ColumnDefinition>) -> SchemaChange {
        SchemaChange::CreateTable {
            container: CatalogPath {
                database: Some("app".to_owned()),
                namespace: namespace.map(str::to_owned),
            },
            name: "order lines".to_owned(),
            columns,
        }
    }

    fn error_message(change: &SchemaChange) -> String {
        let error = change.render().expect_err("the change is incomplete");
        assert_eq!(error.kind, ErrorKind::InvalidRequest);
        error.message
    }

    #[test]
    fn mysql_schema_names_use_backticks() {
        let change = SchemaChange::CreateTable {
            container: CatalogPath::database("shop"),
            name: "Order`Lines".to_owned(),
            columns: vec![key("id", "bigint AUTO_INCREMENT")],
        };
        for engine in [EngineKind::Mysql, EngineKind::Mariadb] {
            let sql = change.render_for_engine(engine).expect("render");
            assert!(sql.starts_with("CREATE TABLE `Order``Lines`"));
            assert!(sql.contains("`id` bigint AUTO_INCREMENT NOT NULL PRIMARY KEY"));
            assert_eq!(
                SchemaChange::CreateDatabase {
                    name: "odd`name".to_owned()
                }
                .render_for_engine(engine)
                .expect("database"),
                "CREATE DATABASE `odd``name`"
            );
        }
    }

    #[test]
    fn creates_a_database_with_a_quoted_name() {
        let change = SchemaChange::CreateDatabase {
            name: "Sales \"2026\"".to_owned(),
        };

        assert_eq!(
            change.render().expect("render"),
            "CREATE DATABASE \"Sales \"\"2026\"\"\""
        );
    }

    #[test]
    fn creates_a_table_with_an_inline_key() {
        let change = create_table(
            Some("sales"),
            vec![
                key("id", "INTEGER"),
                ColumnDefinition {
                    is_nullable: false,
                    default: Some("'new'".to_owned()),
                    ..column("status", "TEXT")
                },
                column("note", "TEXT"),
            ],
        );

        assert_eq!(
            change.render().expect("render"),
            "CREATE TABLE \"sales\".\"order lines\" (\n    \
             \"id\" INTEGER NOT NULL PRIMARY KEY,\n    \
             \"status\" TEXT NOT NULL DEFAULT 'new',\n    \
             \"note\" TEXT\n)"
        );
    }

    #[test]
    fn declares_a_composite_key_as_a_table_constraint() {
        let change = create_table(None, vec![key("order", "integer"), key("line", "integer")]);

        assert_eq!(
            change.render().expect("render"),
            "CREATE TABLE \"order lines\" (\n    \
             \"order\" integer NOT NULL,\n    \
             \"line\" integer NOT NULL,\n    \
             PRIMARY KEY (\"order\", \"line\")\n)"
        );
    }

    #[test]
    fn adds_a_column_to_a_table() {
        let change = SchemaChange::AddColumn {
            table: table(Some("public"), "customers", ObjectKind::Table),
            column: ColumnDefinition {
                is_nullable: false,
                default: Some("  0 ".to_owned()),
                ..column("visits", " integer ")
            },
        };

        assert_eq!(
            change.render().expect("render"),
            "ALTER TABLE \"public\".\"customers\" ADD COLUMN \"visits\" integer NOT NULL DEFAULT 0"
        );
    }

    #[test]
    fn rejects_incomplete_tables() {
        assert_eq!(
            error_message(&create_table(None, Vec::new())),
            "Add at least one column"
        );
        assert_eq!(
            error_message(&create_table(None, vec![column(" ", "TEXT")])),
            "Enter a name for every column"
        );
        assert_eq!(
            error_message(&create_table(None, vec![column("name", "")])),
            "Choose a type for \"name\""
        );
        assert_eq!(
            error_message(&create_table(
                None,
                vec![column("name", "TEXT"), column("name", "TEXT")]
            )),
            "There are two columns named \"name\""
        );
        assert_eq!(
            error_message(&SchemaChange::CreateTable {
                container: CatalogPath::root(),
                name: String::new(),
                columns: vec![column("name", "TEXT")],
            }),
            "Enter a name for the table"
        );
    }

    #[test]
    fn adds_columns_only_to_tables_and_never_as_keys() {
        let to_view = SchemaChange::AddColumn {
            table: table(None, "recent", ObjectKind::View),
            column: column("visits", "integer"),
        };
        let as_key = SchemaChange::AddColumn {
            table: table(None, "customers", ObjectKind::Table),
            column: key("id", "integer"),
        };

        assert!(error_message(&to_view).contains("is a view"));
        assert_eq!(
            error_message(&as_key),
            "Set the primary key when creating the table"
        );
    }
}
