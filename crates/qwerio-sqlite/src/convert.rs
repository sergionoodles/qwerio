//! Conversions between SQLite values and core values. SQLite values keep their
//! per-cell storage class regardless of the column's declared type.

use qwerio_core::value::{UnsupportedValue, Value};
use rusqlite::ToSql;
use rusqlite::types::{ToSqlOutput, ValueRef};

pub(crate) fn value_from_ref(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(integer) => Value::Integer(integer),
        ValueRef::Real(real) => Value::Float(real),
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => Value::Text(text.to_owned()),
            // Keep the exact bytes rather than silently replacing characters.
            Err(_) => Value::Unsupported(UnsupportedValue {
                type_name: "text (invalid UTF-8)".to_owned(),
                text: Some(String::from_utf8_lossy(bytes).into_owned()),
                raw: Some(bytes.to_vec()),
            }),
        },
        ValueRef::Blob(bytes) => Value::Bytes(bytes.to_vec()),
    }
}

/// Binds a core value. Text-like values bind as TEXT so that SQLite applies
/// the compared column's affinity, as it does for literals.
pub(crate) struct SqlValue<'a>(pub &'a Value);

impl ToSql for SqlValue<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        let output = match self.0 {
            Value::Null => ToSqlOutput::Borrowed(ValueRef::Null),
            Value::Bool(value) => ToSqlOutput::Borrowed(ValueRef::Integer(i64::from(*value))),
            Value::Integer(value) => ToSqlOutput::Borrowed(ValueRef::Integer(*value)),
            Value::Float(value) => ToSqlOutput::Borrowed(ValueRef::Real(*value)),
            Value::Decimal(text) | Value::Text(text) | Value::Json(text) | Value::Uuid(text) => {
                ToSqlOutput::Borrowed(ValueRef::Text(text.as_bytes()))
            }
            Value::Temporal(temporal) => {
                ToSqlOutput::Borrowed(ValueRef::Text(temporal.text.as_bytes()))
            }
            Value::Bytes(bytes) => ToSqlOutput::Borrowed(ValueRef::Blob(bytes)),
            Value::Unsupported(UnsupportedValue {
                raw: Some(raw),
                type_name,
                ..
            }) if type_name.starts_with("text") => ToSqlOutput::Borrowed(ValueRef::Text(raw)),
            Value::Unsupported(UnsupportedValue { raw: Some(raw), .. }) => {
                ToSqlOutput::Borrowed(ValueRef::Blob(raw))
            }
            Value::Array(_) | Value::Unsupported(_) => {
                return Err(rusqlite::Error::ToSqlConversionFailure(
                    format!("SQLite cannot bind a {} value", self.0.storage_name()).into(),
                ));
            }
        };

        Ok(output)
    }
}
