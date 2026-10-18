//! Dynamic cell values.
//!
//! Values are canonical: NULL, empty strings, integers, floats, exact decimals,
//! text, and blobs stay distinct, and exact numbers never pass through floating
//! point. Display formatting lives in [`crate::display`] and never replaces the
//! canonical value used for copying or exporting.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    /// Exact decimal text (for example PostgreSQL `numeric`), including values
    /// outside the `i64` range. Never converted through floating point.
    Decimal(String),
    Text(String),
    Bytes(Vec<u8>),
    Temporal(Temporal),
    /// JSON document text exactly as returned by the engine.
    Json(String),
    /// A UUID in canonical lowercase hyphenated form.
    Uuid(String),
    Array(Vec<Value>),
    /// A value of a type this adapter does not decode.
    Unsupported(UnsupportedValue),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TemporalKind {
    Date,
    Time,
    TimeWithZone,
    Timestamp,
    TimestampWithZone,
    Interval,
}

/// A temporal value in ISO 8601 text, preserving the engine's precision.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Temporal {
    pub kind: TemporalKind,
    pub text: String,
}

/// Raw and display fallbacks for an undecoded value, with explicit absence
/// rather than an invented representation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsupportedValue {
    pub type_name: String,
    /// Engine-provided text, when available.
    pub text: Option<String>,
    /// Engine wire bytes, when available.
    pub raw: Option<Vec<u8>>,
}

/// Fixed per-value overhead used when estimating retained memory.
const VALUE_OVERHEAD_BYTES: usize = std::mem::size_of::<Value>();

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// The storage class name, which for SQLite is the per-cell storage type.
    pub fn storage_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Integer(_) => "integer",
            Self::Float(_) => "real",
            Self::Decimal(_) => "decimal",
            Self::Text(_) => "text",
            Self::Bytes(_) => "blob",
            Self::Temporal(_) => "temporal",
            Self::Json(_) => "json",
            Self::Uuid(_) => "uuid",
            Self::Array(_) => "array",
            Self::Unsupported(_) => "unsupported",
        }
    }

    /// Approximate retained memory, used for byte budgets.
    pub fn estimated_bytes(&self) -> usize {
        let payload = match self {
            Self::Null | Self::Bool(_) | Self::Integer(_) | Self::Float(_) => 0,
            Self::Decimal(text) | Self::Text(text) | Self::Json(text) | Self::Uuid(text) => {
                text.len()
            }
            Self::Bytes(bytes) => bytes.len(),
            Self::Temporal(temporal) => temporal.text.len(),
            Self::Array(values) => values.iter().map(Self::estimated_bytes).sum(),
            Self::Unsupported(value) => {
                value.type_name.len()
                    + value.text.as_ref().map_or(0, String::len)
                    + value.raw.as_ref().map_or(0, Vec::len)
            }
        };

        VALUE_OVERHEAD_BYTES + payload
    }

    /// The complete canonical text used for copying and exporting. `None` for
    /// NULL and for unsupported values without engine-provided text; callers
    /// decide how to represent those explicitly.
    pub fn canonical_text(&self) -> Option<String> {
        match self {
            Self::Null => None,
            Self::Bool(value) => Some(value.to_string()),
            Self::Integer(value) => Some(value.to_string()),
            Self::Float(value) => Some(format_float(*value)),
            Self::Decimal(text) | Self::Text(text) | Self::Json(text) | Self::Uuid(text) => {
                Some(text.clone())
            }
            Self::Bytes(bytes) => Some(hex_bytes(bytes)),
            Self::Temporal(temporal) => Some(temporal.text.clone()),
            Self::Array(values) => Some(array_text(values)),
            Self::Unsupported(value) => value.text.clone(),
        }
    }
}

/// Shortest text that parses back to the same `f64`, with SQL-style names for
/// non-finite values.
pub fn format_float(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }

    if value.is_infinite() {
        return if value.is_sign_positive() {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        };
    }

    format!("{value}")
}

/// `\x`-prefixed lowercase hexadecimal, the PostgreSQL `bytea` hex format.
pub fn hex_bytes(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(2 + bytes.len() * 2);
    text.push_str("\\x");

    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }

    text
}

fn array_text(values: &[Value]) -> String {
    let elements: Vec<String> = values
        .iter()
        .map(|value| match value {
            Value::Null => "NULL".to_owned(),
            Value::Text(text) => quote_array_element(text),
            other => other.canonical_text().unwrap_or_else(|| "?".to_owned()),
        })
        .collect();

    format!("{{{}}}", elements.join(","))
}

fn quote_array_element(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_and_empty_text_are_distinct() {
        assert_ne!(Value::Null, Value::Text(String::new()));
        assert_eq!(Value::Null.canonical_text(), None);
        assert_eq!(
            Value::Text(String::new()).canonical_text(),
            Some(String::new())
        );
    }

    #[test]
    fn exact_decimals_keep_their_digits() {
        let digits = "123456789012345678901234567890.000000000000000000001";

        assert_eq!(
            Value::Decimal(digits.to_owned())
                .canonical_text()
                .as_deref(),
            Some(digits)
        );
    }

    #[test]
    fn large_integers_are_not_coerced_to_float() {
        let value = Value::Integer(i64::MAX);

        assert_eq!(
            value.canonical_text().as_deref(),
            Some("9223372036854775807")
        );
    }

    #[test]
    fn floats_round_trip_through_canonical_text() {
        for value in [0.1, 1e300, -2.5e-8, 123_456_789.123_456_78] {
            let text = Value::Float(value).canonical_text().unwrap_or_default();

            assert_eq!(text.parse::<f64>().ok(), Some(value));
        }

        assert_eq!(format_float(f64::NAN), "NaN");
        assert_eq!(format_float(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn blobs_render_as_hex() {
        assert_eq!(hex_bytes(&[0x00, 0xab, 0x10]), "\\x00ab10");
    }

    #[test]
    fn arrays_quote_text_and_mark_nulls() {
        let value = Value::Array(vec![
            Value::Text("a\"b".to_owned()),
            Value::Null,
            Value::Integer(3),
        ]);

        assert_eq!(
            value.canonical_text().as_deref(),
            Some("{\"a\\\"b\",NULL,3}")
        );
    }
}
