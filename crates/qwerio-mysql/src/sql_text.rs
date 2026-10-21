//! Only classifies diagnostics. The server's prepare protocol enforces one statement.

pub(crate) fn statement_count(sql: &str) -> usize {
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut count = 0;
    let mut has_text = false;
    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() => index += 1,
            b';' => {
                count += usize::from(has_text);
                has_text = false;
                index += 1;
            }
            b'#' => index = skip_line(bytes, index),
            b'-' if bytes.get(index + 1) == Some(&b'-')
                && bytes.get(index + 2).is_none_or(u8::is_ascii_whitespace) =>
            {
                index = skip_line(bytes, index)
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                has_text |= bytes.get(index + 2) == Some(&b'!')
                    || bytes.get(index + 2..index + 4) == Some(b"M!");
                index += 2;
                while index + 1 < bytes.len() && &bytes[index..index + 2] != b"*/" {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            quote @ (b'\'' | b'"' | b'`') => {
                has_text = true;
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' && quote != b'`' {
                        index = (index + 2).min(bytes.len());
                        continue;
                    }
                    if bytes[index] == quote {
                        index += 1;
                        if bytes.get(index) != Some(&quote) {
                            break;
                        }
                    }
                    index += 1;
                }
            }
            _ => {
                has_text = true;
                index += 1;
            }
        }
    }
    count + usize::from(has_text)
}

fn skip_line(bytes: &[u8], index: usize) -> usize {
    bytes[index..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |offset| index + offset + 1)
}

/// Only accepts BEGIN [WORK] with ordinary comments and a trailing delimiter.
/// Executable comments are rejected because they may contain additional SQL.
pub(crate) fn is_begin(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut words = Vec::new();
    let mut has_delimiter = false;
    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() => index += 1,
            b'#' => index = skip_line(bytes, index),
            b'-' if bytes.get(index + 1) == Some(&b'-')
                && bytes.get(index + 2).is_none_or(u8::is_ascii_whitespace) =>
            {
                index = skip_line(bytes, index)
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                if bytes.get(index + 2) == Some(&b'!')
                    || bytes.get(index + 2..index + 4) == Some(b"M!")
                {
                    return false;
                }
                index += 2;
                while index + 1 < bytes.len() && &bytes[index..index + 2] != b"*/" {
                    index += 1;
                }
                if index + 1 >= bytes.len() {
                    return false;
                }
                index += 2;
            }
            b';' if !has_delimiter => {
                has_delimiter = true;
                index += 1;
            }
            byte if byte.is_ascii_alphabetic() && !has_delimiter => {
                let start = index;
                while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                    index += 1;
                }
                words.push(sql[start..index].to_ascii_uppercase());
            }
            _ => return false,
        }
    }
    words == ["BEGIN"] || words == ["BEGIN", "WORK"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_fallback_never_forwards_additional_sql() {
        assert!(is_begin("/* note */ BEGIN WORK; -- done"));
        assert!(is_begin("/* My note */ BEGIN;"));
        for sql in [
            "BEGIN; SELECT 1",
            "BEGIN /*! COMMIT */",
            "BEGIN; # hi\nCOMMIT",
            "BEGINNING",
            "BEGIN /* unclosed",
            "BEGIN /*",
        ] {
            assert!(!is_begin(sql), "{sql}");
        }
    }

    #[test]
    fn recognizes_comments_and_quoted_delimiters() {
        assert_eq!(statement_count("SELECT 'a;b', `x;y`; -- done\n # end"), 1);
        assert_eq!(statement_count("# nothing\n/* comment */;"), 0);
        assert_eq!(statement_count("INSERT INTO a VALUES (1); SELECT 1"), 2);
        assert_eq!(statement_count("/*! SELECT 1 */"), 1);
    }
}
