//! Exporting rows as CSV, TSV, JSON, or SQL `INSERT` statements from
//! canonical values. Exports never use display previews, so truncated cells
//! are exported in full.

mod delimited;
mod json;
mod sql_insert;

use std::io::{self, Write};

use crate::catalog::ObjectDetails;
use crate::connection::EngineKind;
use crate::resource::ObjectRef;
use crate::result::{ColumnDescriptor, Row};

use delimited::Dialect;
use sql_insert::InsertStatement;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    /// RFC 4180 CSV. NULL is an empty unquoted field; empty text is `""`.
    Csv(CsvOptions),
    /// Tab-separated values for pasting into spreadsheets. NULL is `\N`.
    Tsv,
    /// An array of objects keyed by column name, one object per line.
    Json,
    /// One `INSERT` statement per row in the source engine's dialect.
    SqlInsert,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv(_) => "csv",
            Self::Tsv => "tsv",
            Self::Json => "json",
            Self::SqlInsert => "sql",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Csv(_) => "CSV",
            Self::Tsv => "TSV",
            Self::Json => "JSON",
            Self::SqlInsert => "SQL INSERT",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CsvOptions {
    pub separator: CsvSeparator,
    /// Whether the first line names the columns.
    pub include_header: bool,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            separator: CsvSeparator::Comma,
            include_header: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CsvSeparator {
    #[default]
    Comma,
    /// Common where the comma is the decimal separator.
    Semicolon,
    Tab,
    Pipe,
}

impl CsvSeparator {
    pub const ALL: [Self; 4] = [Self::Comma, Self::Semicolon, Self::Tab, Self::Pipe];

    pub fn character(self) -> char {
        match self {
            Self::Comma => ',',
            Self::Semicolon => ';',
            Self::Tab => '\t',
            Self::Pipe => '|',
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Comma => "Comma",
            Self::Semicolon => "Semicolon",
            Self::Tab => "Tab",
            Self::Pipe => "Pipe",
        }
    }
}

/// The table or view rows come from. `INSERT` statements target it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportTarget {
    pub engine: EngineKind,
    pub object: ObjectRef,
    /// Columns the database computes, which `INSERT` statements leave out.
    pub generated_columns: Vec<String>,
}

impl ExportTarget {
    pub fn new(engine: EngineKind, details: &ObjectDetails) -> Self {
        Self {
            engine,
            object: details.object.clone(),
            generated_columns: details
                .columns
                .iter()
                .filter(|column| column.generated)
                .map(|column| column.name.clone())
                .collect(),
        }
    }
}

/// Writes one export incrementally, so large exports stream page by page:
/// [`Exporter::write_start`], any number of [`Exporter::write_rows`], then
/// [`Exporter::write_end`].
#[derive(Debug)]
pub struct Exporter {
    format: ExportFormat,
    columns: Vec<ColumnDescriptor>,
    insert: InsertStatement,
    rows_written: u64,
}

impl Exporter {
    pub fn new(
        format: ExportFormat,
        target: &ExportTarget,
        columns: Vec<ColumnDescriptor>,
    ) -> Self {
        Self {
            format,
            insert: InsertStatement::new(target, &columns),
            columns,
            rows_written: 0,
        }
    }

    pub fn rows_written(&self) -> u64 {
        self.rows_written
    }

    pub fn write_start(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        match self.format {
            ExportFormat::Csv(options) if !options.include_header => Ok(()),
            ExportFormat::Csv(options) => {
                delimited::write_header(writer, csv_dialect(options), &self.columns)
            }
            ExportFormat::Tsv => delimited::write_header(writer, Dialect::Tsv, &self.columns),
            ExportFormat::Json => writer.write_all(b"["),
            ExportFormat::SqlInsert => Ok(()),
        }
    }

    pub fn write_rows(&mut self, writer: &mut dyn Write, rows: &[Row]) -> io::Result<()> {
        for row in rows {
            self.write_row(writer, row)?;
            self.rows_written += 1;
        }

        Ok(())
    }

    fn write_row(&self, writer: &mut dyn Write, row: &Row) -> io::Result<()> {
        match self.format {
            ExportFormat::Csv(options) => delimited::write_row(writer, csv_dialect(options), row),
            ExportFormat::Tsv => delimited::write_row(writer, Dialect::Tsv, row),
            ExportFormat::Json => {
                let separator = if self.rows_written == 0 {
                    "\n  "
                } else {
                    ",\n  "
                };
                writer.write_all(separator.as_bytes())?;
                writer.write_all(json::object(&self.columns, row).as_bytes())
            }
            ExportFormat::SqlInsert => {
                writer.write_all(self.insert.statement(row).as_bytes())?;
                writer.write_all(b"\n")
            }
        }
    }

    pub fn write_end(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        match self.format {
            ExportFormat::Json if self.rows_written == 0 => writer.write_all(b"]\n"),
            ExportFormat::Json => writer.write_all(b"\n]\n"),
            ExportFormat::Csv(_) | ExportFormat::Tsv | ExportFormat::SqlInsert => Ok(()),
        }
    }
}

fn csv_dialect(options: CsvOptions) -> Dialect {
    Dialect::Csv {
        separator: options.separator.character(),
    }
}

/// A complete export of rows already in memory.
pub fn export_string(
    format: ExportFormat,
    target: &ExportTarget,
    columns: &[ColumnDescriptor],
    rows: &[Row],
) -> String {
    let mut buffer = Vec::new();
    let mut exporter = Exporter::new(format, target, columns.to_vec());

    let written = exporter
        .write_start(&mut buffer)
        .and_then(|()| exporter.write_rows(&mut buffer, rows))
        .and_then(|()| exporter.write_end(&mut buffer));

    if let Err(error) = written {
        unreachable!("writing to memory cannot fail: {error}");
    }

    String::from_utf8_lossy(&buffer).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ConnectionId;
    use crate::resource::ObjectKind;
    use crate::value::Value;

    fn columns() -> Vec<ColumnDescriptor> {
        vec![
            ColumnDescriptor::new("id", "integer"),
            ColumnDescriptor::new("id", "text"),
        ]
    }

    fn target(engine: EngineKind) -> ExportTarget {
        ExportTarget {
            engine,
            object: ObjectRef {
                connection: ConnectionId::new(),
                database: Some("shop".to_owned()),
                namespace: Some("public".to_owned()),
                kind: ObjectKind::Table,
                name: "order \"items\"".to_owned(),
            },
            generated_columns: Vec::new(),
        }
    }

    fn export(format: ExportFormat, rows: &[Row]) -> String {
        export_string(format, &target(EngineKind::Postgres), &columns(), rows)
    }

    #[test]
    fn csv_distinguishes_null_from_empty_text_and_keeps_duplicate_headers() {
        let rows = vec![vec![Value::Null, Value::Text(String::new())]];

        assert_eq!(
            export(ExportFormat::Csv(CsvOptions::default()), &rows),
            "\"id\",\"id\"\n,\"\"\n"
        );
    }

    #[test]
    fn csv_quotes_embedded_quotes_and_newlines() {
        let rows = vec![vec![
            Value::Integer(1),
            Value::Text("a \"b\"\nc".to_owned()),
        ]];

        assert_eq!(
            export(ExportFormat::Csv(CsvOptions::default()), &rows),
            "\"id\",\"id\"\n\"1\",\"a \"\"b\"\"\nc\"\n"
        );
    }

    #[test]
    fn csv_uses_the_chosen_separator_and_can_leave_out_the_header() {
        let rows = vec![vec![Value::Integer(1), Value::Text("a;b".to_owned())]];
        let semicolons = CsvOptions {
            separator: CsvSeparator::Semicolon,
            include_header: true,
        };
        let headerless_tabs = CsvOptions {
            separator: CsvSeparator::Tab,
            include_header: false,
        };

        assert_eq!(
            export(ExportFormat::Csv(semicolons), &rows),
            "\"id\";\"id\"\n\"1\";\"a;b\"\n"
        );
        assert_eq!(
            export(ExportFormat::Csv(headerless_tabs), &rows),
            "\"1\"\t\"a;b\"\n"
        );
    }

    #[test]
    fn tsv_escapes_control_characters_and_marks_null() {
        let rows = vec![vec![Value::Null, Value::Text("a\tb\\".to_owned())]];

        assert_eq!(export(ExportFormat::Tsv, &rows), "id\tid\n\\N\ta\\tb\\\\\n");
    }

    #[test]
    fn exports_use_full_values_not_previews() {
        let long_text = "x".repeat(10_000);
        let rows = vec![vec![Value::Integer(1), Value::Text(long_text.clone())]];

        for format in [
            ExportFormat::Tsv,
            ExportFormat::Json,
            ExportFormat::SqlInsert,
        ] {
            assert!(export(format, &rows).contains(&long_text));
        }
    }

    #[test]
    fn json_is_an_array_of_objects_with_typed_values() {
        let columns = vec![
            ColumnDescriptor::new("id", "integer"),
            ColumnDescriptor::new("total", "numeric"),
            ColumnDescriptor::new("note", "text"),
            ColumnDescriptor::new("tags", "jsonb"),
        ];
        let rows = vec![
            vec![
                Value::Integer(1),
                Value::Decimal("10.50".to_owned()),
                Value::Text("say \"hi\"".to_owned()),
                Value::Json("{\"a\": [1]}".to_owned()),
            ],
            vec![
                Value::Integer(2),
                Value::Decimal("NaN".to_owned()),
                Value::Null,
                Value::Null,
            ],
        ];

        assert_eq!(
            export_string(
                ExportFormat::Json,
                &target(EngineKind::Postgres),
                &columns,
                &rows
            ),
            concat!(
                "[\n",
                "  {\"id\": 1, \"total\": 10.50, \"note\": \"say \\\"hi\\\"\", \"tags\": {\"a\": [1]}},\n",
                "  {\"id\": 2, \"total\": \"NaN\", \"note\": null, \"tags\": null}\n",
                "]\n"
            )
        );
    }

    #[test]
    fn json_stays_valid_across_batches_and_when_empty() {
        let target = target(EngineKind::Postgres);
        let mut exporter = Exporter::new(ExportFormat::Json, &target, columns());
        let mut output = Vec::new();

        exporter.write_start(&mut output).expect("start");
        exporter
            .write_rows(&mut output, &[vec![Value::Integer(1), Value::Null]])
            .expect("first batch");
        exporter
            .write_rows(&mut output, &[vec![Value::Integer(2), Value::Null]])
            .expect("second batch");
        exporter.write_end(&mut output).expect("end");

        assert_eq!(
            String::from_utf8(output).expect("utf-8"),
            "[\n  {\"id\": 1, \"id\": null},\n  {\"id\": 2, \"id\": null}\n]\n"
        );
        assert_eq!(exporter.rows_written(), 2);
        assert_eq!(export(ExportFormat::Json, &[]), "[]\n");
    }

    #[test]
    fn sql_inserts_quote_names_and_skip_generated_columns() {
        let columns = vec![
            ColumnDescriptor::new("id", "integer"),
            ColumnDescriptor::new("name", "text"),
            ColumnDescriptor::new("name_length", "integer"),
        ];
        let rows = vec![vec![
            Value::Integer(7),
            Value::Text("O'Brien".to_owned()),
            Value::Integer(7),
        ]];
        let mut postgres = target(EngineKind::Postgres);
        postgres.generated_columns = vec!["name_length".to_owned()];
        let mysql = ExportTarget {
            engine: EngineKind::Mysql,
            ..postgres.clone()
        };

        assert_eq!(
            export_string(ExportFormat::SqlInsert, &postgres, &columns, &rows),
            "INSERT INTO \"public\".\"order \"\"items\"\"\" (\"id\", \"name\") VALUES (7, 'O''Brien');\n"
        );
        assert_eq!(
            export_string(ExportFormat::SqlInsert, &mysql, &columns, &rows),
            "INSERT INTO `order \"items\"` (`id`, `name`) VALUES (7, 'O''Brien');\n"
        );
    }

    #[test]
    fn sql_inserts_use_defaults_when_every_column_is_generated() {
        let mut sqlite = target(EngineKind::Sqlite);
        sqlite.generated_columns = vec!["id".to_owned()];

        assert_eq!(
            export_string(
                ExportFormat::SqlInsert,
                &sqlite,
                &[ColumnDescriptor::new("id", "integer")],
                &[vec![Value::Integer(1)]],
            ),
            "INSERT INTO \"order \"\"items\"\"\" DEFAULT VALUES;\n"
        );
    }
}
