//! CSV and TSV records.

use std::io::{self, Write};

use crate::result::{ColumnDescriptor, Row};
use crate::value::Value;

/// Text for unsupported values that carry no engine text.
const UNSUPPORTED_PLACEHOLDER_PREFIX: &str = "<unsupported ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dialect {
    /// RFC 4180 CSV with any separator. NULL is an empty unquoted field;
    /// empty text is `""`.
    Csv { separator: char },
    /// Tab-separated values for pasting into spreadsheets. NULL is `\N`.
    Tsv,
}

impl Dialect {
    fn separator(self) -> char {
        match self {
            Self::Csv { separator } => separator,
            Self::Tsv => '\t',
        }
    }
}

pub(super) fn write_header(
    writer: &mut dyn Write,
    dialect: Dialect,
    columns: &[ColumnDescriptor],
) -> io::Result<()> {
    let header: Vec<String> = columns
        .iter()
        .map(|column| encode_text(dialect, &column.name))
        .collect();

    write_record(writer, dialect, &header)
}

pub(super) fn write_row(writer: &mut dyn Write, dialect: Dialect, row: &Row) -> io::Result<()> {
    let fields: Vec<String> = row
        .iter()
        .map(|value| encode_value(dialect, value))
        .collect();

    write_record(writer, dialect, &fields)
}

fn write_record(writer: &mut dyn Write, dialect: Dialect, fields: &[String]) -> io::Result<()> {
    let separator = dialect.separator().to_string();

    writer.write_all(fields.join(&separator).as_bytes())?;
    writer.write_all(b"\n")
}

fn encode_value(dialect: Dialect, value: &Value) -> String {
    match (dialect, value) {
        (Dialect::Csv { .. }, Value::Null) => String::new(),
        (Dialect::Tsv, Value::Null) => "\\N".to_owned(),
        (_, Value::Unsupported(unsupported)) if unsupported.text.is_none() => encode_text(
            dialect,
            &format!("{UNSUPPORTED_PLACEHOLDER_PREFIX}{}>", unsupported.type_name),
        ),
        (_, other) => encode_text(dialect, &other.canonical_text().unwrap_or_default()),
    }
}

fn encode_text(dialect: Dialect, text: &str) -> String {
    match dialect {
        Dialect::Csv { .. } => format!("\"{}\"", text.replace('"', "\"\"")),
        Dialect::Tsv => text
            .replace('\\', "\\\\")
            .replace('\t', "\\t")
            .replace('\n', "\\n")
            .replace('\r', "\\r"),
    }
}
