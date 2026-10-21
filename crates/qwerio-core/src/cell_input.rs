//! Context-aware cell editing: which editor suits a column, and parsing what
//! the user typed into a value. Parsing catches obvious mistakes early; the
//! database still converts and validates the value when it is saved.

use crate::catalog::ColumnInfo;
use crate::value::{Temporal, TemporalKind, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellEditor {
    /// A checkbox.
    Boolean,
    Integer,
    /// Integers, decimals, and floating point, kept as exact text.
    Number,
    /// A calendar date, `YYYY-MM-DD`.
    Date,
    /// A date and time of day, optionally with a time zone offset.
    DateTime {
        has_zone: bool,
    },
    Uuid,
    /// Multi-line JSON text.
    Json,
    Text,
}

/// Types a single-line text editor would corrupt or that need a special
/// editor this release does not have.
const READ_ONLY_TYPES: &[&str] = &[
    "bytea",
    "blob",
    "tinyblob",
    "mediumblob",
    "longblob",
    "binary",
    "varbinary",
    "bit",
    "geometry",
];

const INTEGER_TYPES: &[&str] = &[
    "integer",
    "int",
    "int2",
    "int4",
    "int8",
    "smallint",
    "bigint",
    "tinyint",
    "mediumint",
    "serial",
    "smallserial",
    "bigserial",
    "unsigned big int",
];

const NUMBER_TYPES: &[&str] = &[
    "numeric",
    "decimal",
    "real",
    "float",
    "float4",
    "float8",
    "double",
    "double precision",
];

/// The editor for a column, from its declared type. `None` when the column
/// cannot be edited inline (binary data and arrays).
pub fn cell_editor(column: &ColumnInfo) -> Option<CellEditor> {
    let type_name = column.type_name.trim().to_ascii_lowercase();
    let base = base_type_name(&type_name);

    if READ_ONLY_TYPES.contains(&base) || type_name.ends_with("[]") {
        return None;
    }

    let editor = if base.starts_with("bool") {
        CellEditor::Boolean
    } else if type_name.contains("unsigned") || type_name.contains("zerofill") {
        CellEditor::Number
    } else if INTEGER_TYPES.contains(&base) {
        CellEditor::Integer
    } else if NUMBER_TYPES.contains(&base) {
        CellEditor::Number
    } else if base == "date" {
        CellEditor::Date
    } else if base.starts_with("timestamp") || base == "datetime" {
        CellEditor::DateTime {
            has_zone: base == "timestamptz" || type_name.contains("with time zone"),
        }
    } else if base == "uuid" {
        CellEditor::Uuid
    } else if base == "json" || base == "jsonb" {
        CellEditor::Json
    } else {
        CellEditor::Text
    };

    Some(editor)
}

/// The type name without modifiers: `numeric(10,2)` becomes `numeric` and
/// `timestamp(3) with time zone` becomes `timestamp`.
fn base_type_name(type_name: &str) -> &str {
    let end = type_name.find('(').unwrap_or(type_name.len());
    let base = type_name[..end].trim();

    if base.starts_with("timestamp") {
        return base.split_whitespace().next().unwrap_or(base);
    }

    base
}

/// The text a text editor starts with: the canonical value, or empty for
/// NULL.
pub fn initial_text(value: &Value) -> String {
    value.canonical_text().unwrap_or_default()
}

/// A checkbox state for a value. SQLite stores booleans as integers or text.
/// `None` for NULL and for values that are not booleans.
pub fn boolean_state(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(state) => Some(*state),
        Value::Integer(number) => Some(*number != 0),
        Value::Text(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" => Some(true),
            "false" | "f" | "0" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Parses editor text into the value to save. Empty text is an empty string
/// for text columns and an error elsewhere: NULL is set explicitly.
pub fn parse_cell_input(editor: CellEditor, input: &str) -> Result<Value, String> {
    let trimmed = input.trim();

    if trimmed.is_empty() && !matches!(editor, CellEditor::Text | CellEditor::Json) {
        return Err("Enter a value, or set the cell to NULL".to_owned());
    }

    match editor {
        CellEditor::Boolean => boolean_state(&Value::Text(trimmed.to_owned()))
            .map(Value::Bool)
            .ok_or_else(|| "Enter true or false".to_owned()),
        CellEditor::Integer => trimmed
            .parse::<i64>()
            .map(Value::Integer)
            .map_err(|_| "Enter a whole number".to_owned()),
        CellEditor::Number if is_number(trimmed) => Ok(Value::Decimal(trimmed.to_owned())),
        CellEditor::Number => Err("Enter a number".to_owned()),
        CellEditor::Date if is_date(trimmed) => Ok(temporal(TemporalKind::Date, trimmed)),
        CellEditor::Date => Err("Enter a date as YYYY-MM-DD".to_owned()),
        CellEditor::DateTime { has_zone } => parse_date_time(trimmed, has_zone),
        CellEditor::Uuid if is_uuid(trimmed) => Ok(Value::Uuid(trimmed.to_ascii_lowercase())),
        CellEditor::Uuid => {
            Err("Enter a UUID like 123e4567-e89b-12d3-a456-426614174000".to_owned())
        }
        CellEditor::Json => Ok(Value::Json(input.to_owned())),
        CellEditor::Text => Ok(Value::Text(input.to_owned())),
    }
}

fn temporal(kind: TemporalKind, text: &str) -> Value {
    Value::Temporal(Temporal {
        kind,
        text: text.to_owned(),
    })
}

/// Decimal notation with an optional sign, fraction, and exponent, or one of
/// the floating point names databases accept.
fn is_number(text: &str) -> bool {
    if matches!(
        text.to_ascii_lowercase().as_str(),
        "nan" | "infinity" | "+infinity" | "-infinity"
    ) {
        return true;
    }

    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(index) => (&unsigned[..index], Some(&unsigned[index + 1..])),
        None => (unsigned, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let is_digits = |part: &str| part.chars().all(|character| character.is_ascii_digit());
    let has_mantissa =
        !(whole.is_empty() && fraction.is_empty()) && is_digits(whole) && is_digits(fraction);
    let has_valid_exponent = exponent.is_none_or(|exponent| {
        let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !digits.is_empty() && is_digits(digits)
    });

    has_mantissa && has_valid_exponent
}

/// `YYYY-MM-DD` naming a real calendar day.
pub fn is_date(text: &str) -> bool {
    parse_date(text).is_some()
}

/// Year, month, and day of a `YYYY-MM-DD` date that exists.
pub fn parse_date(text: &str) -> Option<(i32, u32, u32)> {
    let mut parts = text.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);

    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }

    let year: i32 = year.parse().ok()?;
    let month: u32 = month.parse().ok()?;
    let day: u32 = day.parse().ok()?;

    (1..=12)
        .contains(&month)
        .then(|| days_in_month(year, month))
        .filter(|days| (1..=*days).contains(&day))
        .map(|_| (year, month, day))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    const DAYS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap_year = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;

    if month == 2 && is_leap_year {
        return 29;
    }

    DAYS[(month - 1) as usize]
}

/// `YYYY-MM-DD HH:MM[:SS[.fraction]]` (a `T` separator also works), with a
/// trailing zone offset or `Z` allowed only for zoned columns.
fn parse_date_time(text: &str, has_zone: bool) -> Result<Value, String> {
    const FORMAT_HINT: &str = "Enter a date and time as YYYY-MM-DD HH:MM:SS";

    let (date, rest) = match text.split_once([' ', 'T']) {
        Some((date, rest)) => (date, rest.trim()),
        None => (text, ""),
    };

    if !is_date(date) {
        return Err(FORMAT_HINT.to_owned());
    }

    let (time, zone) = split_zone(rest);

    if !time.is_empty() && !is_time(time) {
        return Err(FORMAT_HINT.to_owned());
    }

    if !zone.is_empty() && !has_zone {
        return Err("This column stores no time zone; remove the offset".to_owned());
    }

    let kind = if has_zone {
        TemporalKind::TimestampWithZone
    } else {
        TemporalKind::Timestamp
    };

    Ok(temporal(kind, text))
}

/// Splits `HH:MM:SS+02:00` into the time and the zone (`Z`, `+HH`, `-HH:MM`).
fn split_zone(text: &str) -> (&str, &str) {
    if let Some(time) = text.strip_suffix(['Z', 'z']) {
        return (time.trim(), "Z");
    }

    match text.rfind(['+', '-']) {
        Some(index) => (text[..index].trim(), &text[index..]),
        None => (text, ""),
    }
}

/// `HH:MM`, `HH:MM:SS`, or `HH:MM:SS.fraction`.
fn is_time(text: &str) -> bool {
    let (clock, fraction) = text.split_once('.').unwrap_or((text, "0"));
    let parts: Vec<&str> = clock.split(':').collect();
    let limits: &[u32] = &[23, 59, 59];

    let is_clock = (2..=3).contains(&parts.len())
        && parts.iter().zip(limits).all(|(part, limit)| {
            part.len() == 2 && part.parse::<u32>().is_ok_and(|value| value <= *limit)
        });
    let is_fraction = !fraction.is_empty() && fraction.chars().all(|c| c.is_ascii_digit());

    is_clock && is_fraction
}

/// Whether text is a hyphenated or plain 32-digit hexadecimal UUID.
fn is_uuid(text: &str) -> bool {
    uuid::Uuid::try_parse(text).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(type_name: &str) -> ColumnInfo {
        ColumnInfo {
            name: "c".to_owned(),
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

    #[test]
    fn mysql_unsigned_keys_use_exact_numeric_input() {
        for type_name in [
            "bigint unsigned",
            "bigint(20) unsigned",
            "int unsigned",
            "int zerofill",
        ] {
            assert_eq!(cell_editor(&column(type_name)), Some(CellEditor::Number));
        }
        assert_eq!(
            parse_cell_input(CellEditor::Number, "18446744073709551615").expect("unsigned"),
            Value::Decimal("18446744073709551615".to_owned())
        );
        for type_name in [
            "tinyblob",
            "mediumblob",
            "longblob",
            "varbinary(10)",
            "bit(8)",
        ] {
            assert_eq!(cell_editor(&column(type_name)), None);
        }
    }

    #[test]
    fn editors_follow_postgres_and_sqlite_type_names() {
        let cases = [
            ("boolean", Some(CellEditor::Boolean)),
            ("BOOLEAN", Some(CellEditor::Boolean)),
            ("bigint", Some(CellEditor::Integer)),
            ("INTEGER", Some(CellEditor::Integer)),
            ("numeric(10,2)", Some(CellEditor::Number)),
            ("double precision", Some(CellEditor::Number)),
            ("date", Some(CellEditor::Date)),
            ("DATETIME", Some(CellEditor::DateTime { has_zone: false })),
            (
                "timestamp(3) with time zone",
                Some(CellEditor::DateTime { has_zone: true }),
            ),
            (
                "timestamp without time zone",
                Some(CellEditor::DateTime { has_zone: false }),
            ),
            ("uuid", Some(CellEditor::Uuid)),
            ("jsonb", Some(CellEditor::Json)),
            ("character varying(40)", Some(CellEditor::Text)),
            ("point", Some(CellEditor::Text)),
            ("", Some(CellEditor::Text)),
            ("bytea", None),
            ("BLOB", None),
            ("integer[]", None),
        ];

        for (type_name, expected) in cases {
            assert_eq!(cell_editor(&column(type_name)), expected, "{type_name}");
        }
    }

    #[test]
    fn booleans_are_read_from_engine_representations() {
        assert_eq!(boolean_state(&Value::Bool(true)), Some(true));
        assert_eq!(boolean_state(&Value::Integer(0)), Some(false));
        assert_eq!(boolean_state(&Value::Text("t".to_owned())), Some(true));
        assert_eq!(boolean_state(&Value::Null), None);
        assert_eq!(boolean_state(&Value::Text("maybe".to_owned())), None);
    }

    #[test]
    fn numbers_are_validated_and_kept_exact() {
        assert_eq!(
            parse_cell_input(CellEditor::Integer, " 42 "),
            Ok(Value::Integer(42))
        );
        assert!(parse_cell_input(CellEditor::Integer, "4.2").is_err());
        assert_eq!(
            parse_cell_input(CellEditor::Number, "12345678901234567890.123"),
            Ok(Value::Decimal("12345678901234567890.123".to_owned()))
        );
        assert!(parse_cell_input(CellEditor::Number, "-1.5e-3").is_ok());
        assert!(parse_cell_input(CellEditor::Number, "1.2.3").is_err());
        assert!(parse_cell_input(CellEditor::Number, ".").is_err());
    }

    #[test]
    fn dates_must_exist() {
        assert!(parse_cell_input(CellEditor::Date, "2024-02-29").is_ok());
        assert!(parse_cell_input(CellEditor::Date, "2023-02-29").is_err());
        assert!(parse_cell_input(CellEditor::Date, "2024-13-01").is_err());
        assert!(parse_cell_input(CellEditor::Date, "24-01-01").is_err());
    }

    #[test]
    fn date_times_accept_zones_only_for_zoned_columns() {
        let plain = CellEditor::DateTime { has_zone: false };
        let zoned = CellEditor::DateTime { has_zone: true };

        assert!(parse_cell_input(plain, "2024-05-01 13:45").is_ok());
        assert!(parse_cell_input(plain, "2024-05-01T13:45:10.123").is_ok());
        assert!(parse_cell_input(plain, "2024-05-01").is_ok());
        assert!(parse_cell_input(plain, "2024-05-01 13:45+02:00").is_err());
        assert!(parse_cell_input(plain, "2024-05-01 25:00").is_err());
        assert!(parse_cell_input(zoned, "2024-05-01 13:45:00+02:00").is_ok());
        assert!(parse_cell_input(zoned, "2024-05-01T13:45:00Z").is_ok());
    }

    #[test]
    fn empty_input_is_empty_text_but_an_error_for_typed_columns() {
        assert_eq!(
            parse_cell_input(CellEditor::Text, ""),
            Ok(Value::Text(String::new()))
        );
        assert!(parse_cell_input(CellEditor::Integer, "  ").is_err());
        assert!(parse_cell_input(CellEditor::Date, "").is_err());
    }

    #[test]
    fn uuids_are_validated_and_normalized() {
        assert_eq!(
            parse_cell_input(CellEditor::Uuid, "123E4567-E89B-12D3-A456-426614174000"),
            Ok(Value::Uuid(
                "123e4567-e89b-12d3-a456-426614174000".to_owned()
            ))
        );
        assert!(parse_cell_input(CellEditor::Uuid, "not-a-uuid").is_err());
    }
}
