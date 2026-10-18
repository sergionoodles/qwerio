//! Display formatting for cells. A display string is a preview: it may be
//! truncated or single-lined and must never be used as the source for copying
//! or saving data. Use [`Value::canonical_text`] or [`crate::export`] instead.

use crate::value::{Value, hex_bytes};

/// Default maximum number of characters shown in one grid cell.
pub const DEFAULT_CELL_PREVIEW_CHARS: usize = 256;

/// Number of blob bytes shown before the preview is truncated.
const BLOB_PREVIEW_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellStyle {
    /// SQL NULL, rendered distinctly from an empty string.
    Null,
    Text,
    /// Right-aligned numeric values.
    Number,
    Boolean,
    Binary,
    /// A value whose type is not decoded; shown with its type name.
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellPreview {
    pub text: String,
    pub style: CellStyle,
    pub is_truncated: bool,
}

pub const NULL_LABEL: &str = "NULL";

/// Formats a value for one grid cell: newlines and tabs become visible
/// symbols, and long text is truncated to `max_chars` characters.
pub fn preview_cell(value: &Value, max_chars: usize) -> CellPreview {
    match value {
        Value::Null => CellPreview {
            text: NULL_LABEL.to_owned(),
            style: CellStyle::Null,
            is_truncated: false,
        },
        Value::Bool(_) => full_preview(value, CellStyle::Boolean),
        Value::Integer(_) | Value::Float(_) | Value::Decimal(_) => {
            full_preview(value, CellStyle::Number)
        }
        Value::Bytes(bytes) => preview_bytes(bytes),
        Value::Unsupported(unsupported) => {
            let text = match &unsupported.text {
                Some(text) => truncate_single_line(text, max_chars).0,
                None => format!("<{}>", unsupported.type_name),
            };

            CellPreview {
                text,
                style: CellStyle::Unsupported,
                is_truncated: false,
            }
        }
        _ => {
            let text = value.canonical_text().unwrap_or_default();
            let (text, is_truncated) = truncate_single_line(&text, max_chars);

            CellPreview {
                text,
                style: CellStyle::Text,
                is_truncated,
            }
        }
    }
}

fn full_preview(value: &Value, style: CellStyle) -> CellPreview {
    CellPreview {
        text: value.canonical_text().unwrap_or_default(),
        style,
        is_truncated: false,
    }
}

fn preview_bytes(bytes: &[u8]) -> CellPreview {
    let shown = &bytes[..bytes.len().min(BLOB_PREVIEW_BYTES)];
    let is_truncated = shown.len() < bytes.len();
    let mut text = hex_bytes(shown);

    if is_truncated {
        text.push('…');
    }

    CellPreview {
        text: format!("{text} ({})", format_byte_size(bytes.len())),
        style: CellStyle::Binary,
        is_truncated,
    }
}

fn truncate_single_line(text: &str, max_chars: usize) -> (String, bool) {
    let mut output = String::with_capacity(text.len().min(max_chars * 4));
    let mut is_truncated = false;

    for (count, character) in text.chars().enumerate() {
        if count == max_chars {
            is_truncated = true;
            break;
        }

        match character {
            '\n' => output.push('↵'),
            '\r' => {}
            '\t' => output.push('→'),
            other => output.push(other),
        }
    }

    if is_truncated {
        output.push('…');
    }

    (output, is_truncated)
}

/// Human-readable size using binary units.
pub fn format_byte_size(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    const STEP: f64 = 1024.0;

    let mut size = bytes as f64;
    let mut unit = 0;

    while size >= STEP && unit < UNITS.len() - 1 {
        size /= STEP;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::UnsupportedValue;

    #[test]
    fn null_preview_differs_from_empty_text() {
        let null = preview_cell(&Value::Null, 10);
        let empty = preview_cell(&Value::Text(String::new()), 10);

        assert_eq!(null.style, CellStyle::Null);
        assert_eq!(empty.style, CellStyle::Text);
        assert_eq!(empty.text, "");
    }

    #[test]
    fn long_text_is_truncated_and_flagged() {
        let preview = preview_cell(&Value::Text("abcdef".to_owned()), 3);

        assert_eq!(preview.text, "abc…");
        assert!(preview.is_truncated);
    }

    #[test]
    fn newlines_are_visible_in_previews() {
        let preview = preview_cell(&Value::Text("a\nb".to_owned()), 10);

        assert_eq!(preview.text, "a↵b");
        assert!(!preview.is_truncated);
    }

    #[test]
    fn unsupported_values_show_type_without_inventing_text() {
        let value = Value::Unsupported(UnsupportedValue {
            type_name: "tsvector".to_owned(),
            text: None,
            raw: Some(vec![1, 2]),
        });

        assert_eq!(preview_cell(&value, 10).text, "<tsvector>");
    }

    #[test]
    fn byte_sizes_use_binary_units() {
        assert_eq!(format_byte_size(512), "512 B");
        assert_eq!(format_byte_size(1536), "1.5 KiB");
    }
}
