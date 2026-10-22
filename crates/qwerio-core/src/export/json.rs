//! JSON objects keyed by column name. Numbers stay numbers only when their
//! canonical text is a valid JSON number, so exact decimals keep every digit
//! and non-finite floats become strings instead of invalid JSON.

use crate::result::{ColumnDescriptor, Row};
use crate::value::{Value, format_float, hex_bytes};

pub(super) fn object(columns: &[ColumnDescriptor], row: &Row) -> String {
    let mut text = String::from("{");

    for (index, (column, value)) in columns.iter().zip(row).enumerate() {
        if index > 0 {
            text.push_str(", ");
        }

        push_string(&mut text, &column.name);
        text.push_str(": ");
        push_value(&mut text, value);
    }

    text.push('}');
    text
}

fn push_value(text: &mut String, value: &Value) {
    match value {
        Value::Null => text.push_str("null"),
        Value::Bool(value) => text.push_str(if *value { "true" } else { "false" }),
        Value::Integer(value) => text.push_str(&value.to_string()),
        Value::Float(value) => push_number_or_string(text, &format_float(*value)),
        Value::Decimal(digits) => push_number_or_string(text, digits),
        // The engine validated the document, so it is embedded as is.
        Value::Json(document) => text.push_str(document),
        Value::Text(value) | Value::Uuid(value) => push_string(text, value),
        Value::Temporal(temporal) => push_string(text, &temporal.text),
        Value::Bytes(bytes) => push_string(text, &hex_bytes(bytes)),
        Value::Array(values) => push_array(text, values),
        Value::Unsupported(unsupported) => match &unsupported.text {
            Some(value) => push_string(text, value),
            None => text.push_str("null"),
        },
    }
}

fn push_array(text: &mut String, values: &[Value]) {
    text.push('[');

    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            text.push_str(", ");
        }

        push_value(text, value);
    }

    text.push(']');
}

fn push_number_or_string(text: &mut String, number: &str) {
    if is_number(number) {
        text.push_str(number);
    } else {
        push_string(text, number);
    }
}

fn push_string(text: &mut String, value: &str) {
    text.push('"');

    for character in value.chars() {
        match character {
            '"' => text.push_str("\\\""),
            '\\' => text.push_str("\\\\"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            control if u32::from(control) < 0x20 => {
                text.push_str(&format!("\\u{:04x}", u32::from(control)));
            }
            other => text.push(other),
        }
    }

    text.push('"');
}

/// Whether `text` is a number in JSON's grammar, which SQL accepts as a
/// numeric literal too: an optional minus, an integer part without leading
/// zeros, an optional fraction, and an optional exponent.
pub(super) fn is_number(text: &str) -> bool {
    let text = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(position) => (&text[..position], Some(&text[position + 1..])),
        None => (text, None),
    };
    let (integer, fraction) = match mantissa.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (mantissa, None),
    };

    let is_digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let is_integer_valid = is_digits(integer) && (integer == "0" || !integer.starts_with('0'));
    let is_fraction_valid = fraction.is_none_or(is_digits);
    let is_exponent_valid = exponent
        .is_none_or(|exponent| is_digits(exponent.strip_prefix(['+', '-']).unwrap_or(exponent)));

    is_integer_valid && is_fraction_valid && is_exponent_valid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_json_numbers_only() {
        for number in [
            "0",
            "-1",
            "10.50",
            "1e300",
            "2.5E-8",
            "123456789012345678901234567890",
        ] {
            assert!(is_number(number), "{number}");
        }

        for other in [
            "", "-", "007", ".5", "5.", "NaN", "Infinity", "1e", "0x10", "1 ",
        ] {
            assert!(!is_number(other), "{other}");
        }
    }

    #[test]
    fn strings_escape_quotes_backslashes_and_control_characters() {
        let mut text = String::new();
        push_string(&mut text, "a\"b\\c\nd\u{1}");

        assert_eq!(text, "\"a\\\"b\\\\c\\nd\\u0001\"");
    }
}
