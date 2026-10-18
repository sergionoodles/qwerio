//! Delimited text export from canonical values. Exports never use display
//! previews, so truncated cells are exported in full.

use std::io::{self, Write};

use crate::result::{ColumnDescriptor, Row};
use crate::value::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    /// RFC 4180 CSV. NULL is an empty unquoted field; empty text is `""`.
    Csv,
    /// Tab-separated values for pasting into spreadsheets. NULL is `\N`.
    Tsv,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Tsv => "tsv",
        }
    }
}

/// Text for unsupported values that carry no engine text.
const UNSUPPORTED_PLACEHOLDER_PREFIX: &str = "<unsupported ";

pub fn write_delimited(
    writer: &mut dyn Write,
    format: ExportFormat,
    columns: &[ColumnDescriptor],
    rows: &[Row],
) -> io::Result<()> {
    write_header(writer, format, columns)?;
    write_rows(writer, format, rows)
}

pub fn write_header(
    writer: &mut dyn Write,
    format: ExportFormat,
    columns: &[ColumnDescriptor],
) -> io::Result<()> {
    let header: Vec<String> = columns
        .iter()
        .map(|column| encode_text(format, &column.name))
        .collect();

    write_record(writer, format, &header)
}

pub fn write_rows(writer: &mut dyn Write, format: ExportFormat, rows: &[Row]) -> io::Result<()> {
    for row in rows {
        let fields: Vec<String> = row
            .iter()
            .map(|value| encode_value(format, value))
            .collect();
        write_record(writer, format, &fields)?;
    }

    Ok(())
}

pub fn delimited_string(
    format: ExportFormat,
    columns: &[ColumnDescriptor],
    rows: &[Row],
) -> String {
    let mut buffer = Vec::new();

    if let Err(error) = write_delimited(&mut buffer, format, columns, rows) {
        unreachable!("writing to memory cannot fail: {error}");
    }

    String::from_utf8_lossy(&buffer).into_owned()
}

fn write_record(writer: &mut dyn Write, format: ExportFormat, fields: &[String]) -> io::Result<()> {
    let separator = match format {
        ExportFormat::Csv => ",",
        ExportFormat::Tsv => "\t",
    };

    writer.write_all(fields.join(separator).as_bytes())?;
    writer.write_all(b"\n")
}

fn encode_value(format: ExportFormat, value: &Value) -> String {
    match (format, value) {
        (ExportFormat::Csv, Value::Null) => String::new(),
        (ExportFormat::Tsv, Value::Null) => "\\N".to_owned(),
        (_, Value::Unsupported(unsupported)) if unsupported.text.is_none() => encode_text(
            format,
            &format!("{UNSUPPORTED_PLACEHOLDER_PREFIX}{}>", unsupported.type_name),
        ),
        (_, other) => encode_text(format, &other.canonical_text().unwrap_or_default()),
    }
}

fn encode_text(format: ExportFormat, text: &str) -> String {
    match format {
        ExportFormat::Csv => format!("\"{}\"", text.replace('"', "\"\"")),
        ExportFormat::Tsv => text
            .replace('\\', "\\\\")
            .replace('\t', "\\t")
            .replace('\n', "\\n")
            .replace('\r', "\\r"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns() -> Vec<ColumnDescriptor> {
        vec![
            ColumnDescriptor::new("id", "integer"),
            ColumnDescriptor::new("id", "text"),
        ]
    }

    #[test]
    fn csv_distinguishes_null_from_empty_text_and_keeps_duplicate_headers() {
        let rows = vec![vec![Value::Null, Value::Text(String::new())]];

        assert_eq!(
            delimited_string(ExportFormat::Csv, &columns(), &rows),
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
            delimited_string(ExportFormat::Csv, &columns(), &rows),
            "\"id\",\"id\"\n\"1\",\"a \"\"b\"\"\nc\"\n"
        );
    }

    #[test]
    fn tsv_escapes_control_characters_and_marks_null() {
        let rows = vec![vec![Value::Null, Value::Text("a\tb\\".to_owned())]];

        assert_eq!(
            delimited_string(ExportFormat::Tsv, &columns(), &rows),
            "id\tid\n\\N\ta\\tb\\\\\n"
        );
    }

    #[test]
    fn exports_use_full_values_not_previews() {
        let long_text = "x".repeat(10_000);
        let rows = vec![vec![Value::Integer(1), Value::Text(long_text.clone())]];
        let output = delimited_string(ExportFormat::Tsv, &columns(), &rows);

        assert!(output.contains(&long_text));
    }
}
