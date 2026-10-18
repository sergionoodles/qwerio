//! Lexical helpers for user SQL text. These never split or rewrite SQL; they
//! only classify the start of a single statement.

/// The first keyword of `sql`, uppercased, skipping whitespace, semicolons,
/// `--` line comments, and (nested) `/* */` block comments. `None` when the
/// text contains no statement.
pub fn leading_keyword(sql: &str) -> Option<String> {
    let start = skip_trivia(sql)?;
    let keyword: String = sql[start..]
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect();

    if keyword.is_empty() {
        // The statement starts with a symbol, such as a parenthesized query.
        return Some(String::new());
    }

    Some(keyword.to_ascii_uppercase())
}

/// Whether `sql` contains only whitespace, semicolons, and comments.
pub fn is_blank(sql: &str) -> bool {
    skip_trivia(sql).is_none()
}

fn skip_trivia(sql: &str) -> Option<usize> {
    let bytes = sql.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() || byte == b';' => index += 1,
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index = bytes[index..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |offset| index + offset + 1);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => index = skip_block_comment(bytes, index),
            _ => return Some(index),
        }
    }

    None
}

fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0;
    let mut index = start;

    while index < bytes.len() {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            depth += 1;
            index += 2;
        } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
            depth -= 1;
            index += 2;

            if depth == 0 {
                return index;
            }
        } else {
            index += 1;
        }
    }

    bytes.len()
}

/// Statements that may change transaction state, after which an adapter must
/// re-check the session's actual state.
pub fn may_change_transaction_state(keyword: &str) -> bool {
    matches!(
        keyword,
        "BEGIN"
            | "START"
            | "COMMIT"
            | "END"
            | "ROLLBACK"
            | "ABORT"
            | "SAVEPOINT"
            | "RELEASE"
            | "PREPARE"
            | "CALL"
            | "DO"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_comments_and_whitespace() {
        let sql = "  -- note\n /* outer /* inner */ */ select 1";

        assert_eq!(leading_keyword(sql).as_deref(), Some("SELECT"));
    }

    #[test]
    fn comment_only_text_is_blank() {
        assert!(is_blank(" ;; -- only a comment\n/* and another */ "));
        assert!(is_blank(""));
        assert!(!is_blank("/* c */ VALUES (1)"));
    }

    #[test]
    fn statements_starting_with_symbols_are_not_blank() {
        assert_eq!(leading_keyword("(SELECT 1)").as_deref(), Some(""));
    }

    #[test]
    fn transaction_keywords_are_recognized() {
        assert!(may_change_transaction_state("COMMIT"));
        assert!(!may_change_transaction_state("SELECT"));
    }
}
