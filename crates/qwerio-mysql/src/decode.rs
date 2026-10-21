//! Decode protocol values without routing exact numbers through floating point.

use mysql_async::consts::ColumnType;
use mysql_async::{Column, Row, Value as MysqlValue};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::result::ColumnDescriptor;
use qwerio_core::value::{Temporal, TemporalKind, UnsupportedValue, Value};

pub(crate) fn columns(columns: &[Column]) -> Vec<ColumnDescriptor> {
    columns
        .iter()
        .map(|column| ColumnDescriptor::new(column.name_str(), type_name(column.column_type())))
        .collect()
}

fn type_name(kind: ColumnType) -> &'static str {
    use ColumnType::*;
    match kind {
        MYSQL_TYPE_TINY => "tinyint",
        MYSQL_TYPE_SHORT => "smallint",
        MYSQL_TYPE_LONG => "int",
        MYSQL_TYPE_INT24 => "mediumint",
        MYSQL_TYPE_LONGLONG => "bigint",
        MYSQL_TYPE_YEAR => "year",
        MYSQL_TYPE_FLOAT => "float",
        MYSQL_TYPE_DOUBLE => "double",
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "decimal",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "date",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "time",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "datetime",
        MYSQL_TYPE_JSON => "json",
        MYSQL_TYPE_BIT => "bit",
        MYSQL_TYPE_GEOMETRY => "geometry",
        MYSQL_TYPE_NULL => "null",
        MYSQL_TYPE_BLOB | MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB => {
            "blob"
        }
        _ => "text",
    }
}

pub(crate) fn decode_row(row: Row) -> Vec<Value> {
    let columns = row.columns_ref().to_vec();
    row.unwrap()
        .into_iter()
        .zip(&columns)
        .map(|(value, column)| decode(value, column))
        .collect()
}

fn decode(value: MysqlValue, column: &Column) -> Value {
    match value {
        MysqlValue::NULL => Value::Null,
        MysqlValue::Int(value) => Value::Integer(value),
        MysqlValue::UInt(value) => i64::try_from(value)
            .map(Value::Integer)
            .unwrap_or_else(|_| Value::Decimal(value.to_string())),
        MysqlValue::Float(value) => Value::Float(f64::from(value)),
        MysqlValue::Double(value) => Value::Float(value),
        MysqlValue::Bytes(bytes) => decode_bytes(bytes, column),
        MysqlValue::Date(year, month, day, hour, minute, second, micros) => {
            let is_date = matches!(
                column.column_type(),
                ColumnType::MYSQL_TYPE_DATE | ColumnType::MYSQL_TYPE_NEWDATE
            );
            let date = format!("{year:04}-{month:02}-{day:02}");
            let text = if is_date {
                date
            } else {
                format!(
                    "{date} {hour:02}:{minute:02}:{second:02}{}",
                    fraction(micros, column.decimals())
                )
            };
            Value::Temporal(Temporal {
                kind: if is_date {
                    TemporalKind::Date
                } else {
                    TemporalKind::Timestamp
                },
                text,
            })
        }
        MysqlValue::Time(negative, days, hours, minutes, seconds, micros) => {
            let sign = if negative { "-" } else { "" };
            Value::Temporal(Temporal {
                kind: TemporalKind::Time,
                text: format!(
                    "{sign}{:02}:{minutes:02}:{seconds:02}{}",
                    u64::from(days) * 24 + u64::from(hours),
                    fraction(micros, column.decimals())
                ),
            })
        }
    }
}

fn fraction(micros: u32, precision: u8) -> String {
    let precision = usize::from(precision.min(6));
    if precision == 0 {
        return String::new();
    }
    format!(".{:06}", micros)[..precision + 1].to_owned()
}

fn decode_bytes(bytes: Vec<u8>, column: &Column) -> Value {
    if column.character_set() == 63
        && !matches!(
            column.column_type(),
            ColumnType::MYSQL_TYPE_DECIMAL
                | ColumnType::MYSQL_TYPE_NEWDECIMAL
                | ColumnType::MYSQL_TYPE_JSON
        )
    {
        return Value::Bytes(bytes);
    }
    match String::from_utf8(bytes) {
        Ok(text) => match column.column_type() {
            ColumnType::MYSQL_TYPE_DECIMAL | ColumnType::MYSQL_TYPE_NEWDECIMAL => {
                Value::Decimal(text)
            }
            ColumnType::MYSQL_TYPE_JSON => Value::Json(text),
            _ => Value::Text(text),
        },
        Err(error) => Value::Unsupported(UnsupportedValue {
            type_name: type_name(column.column_type()).to_owned(),
            text: None,
            raw: Some(error.into_bytes()),
        }),
    }
}

pub(crate) fn parameters(values: &[Value]) -> DbResult<Vec<MysqlValue>> {
    values
        .iter()
        .map(|value| {
            Ok(match value {
                Value::Null => MysqlValue::NULL,
                Value::Bool(value) => MysqlValue::Int(i64::from(*value)),
                Value::Integer(value) => MysqlValue::Int(*value),
                Value::Float(value) => MysqlValue::Double(*value),
                Value::Bytes(bytes) => MysqlValue::Bytes(bytes.clone()),
                Value::Decimal(text)
                | Value::Text(text)
                | Value::Json(text)
                | Value::Uuid(text) => MysqlValue::Bytes(text.as_bytes().to_vec()),
                Value::Temporal(value) => MysqlValue::Bytes(value.text.as_bytes().to_vec()),
                Value::Unsupported(value) if value.text.is_some() => MysqlValue::Bytes(
                    value
                        .text
                        .as_deref()
                        .unwrap_or_default()
                        .as_bytes()
                        .to_vec(),
                ),
                _ => {
                    return Err(DatabaseError::new(
                        ErrorKind::Parameters,
                        "This value cannot be bound to a MySQL parameter",
                    ));
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_integers_remain_exact() {
        let column = Column::new(ColumnType::MYSQL_TYPE_LONGLONG);
        assert_eq!(
            decode(MysqlValue::UInt(u64::MAX), &column),
            Value::Decimal(u64::MAX.to_string())
        );
    }

    #[test]
    fn parameters_preserve_null_binary_and_exact_decimal() {
        assert_eq!(
            parameters(&[
                Value::Null,
                Value::Bytes(vec![0, 255]),
                Value::Decimal("18446744073709551615".to_owned())
            ])
            .expect("parameters"),
            vec![
                MysqlValue::NULL,
                MysqlValue::Bytes(vec![0, 255]),
                MysqlValue::Bytes(b"18446744073709551615".to_vec())
            ]
        );
        assert!(parameters(&[Value::Array(Vec::new())]).is_err());
    }
}
