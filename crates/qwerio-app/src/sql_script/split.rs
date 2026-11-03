use qwerio_core::connection::EngineKind;
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};

use super::ScriptStatement;

pub(super) fn statements(sql: &str, engine: EngineKind) -> DbResult<Vec<ScriptStatement>> {
    Splitter::new(sql, engine).split()
}

struct Splitter<'a> {
    sql: &'a str,
    engine: EngineKind,
    delimiter: String,
    start: usize,
    has_text: bool,
    keywords: Vec<String>,
    trigger_depth: usize,
    statements: Vec<ScriptStatement>,
}

impl<'a> Splitter<'a> {
    fn new(sql: &'a str, engine: EngineKind) -> Self {
        Self {
            sql,
            engine,
            delimiter: ";".to_owned(),
            start: 0,
            has_text: false,
            keywords: Vec::new(),
            trigger_depth: 0,
            statements: Vec::new(),
        }
    }

    fn is_mysql(&self) -> bool {
        matches!(self.engine, EngineKind::Mysql | EngineKind::Mariadb)
    }

    fn split(mut self) -> DbResult<Vec<ScriptStatement>> {
        let bytes = self.sql.as_bytes();
        let mut index = 0;

        while index < bytes.len() {
            let byte = bytes[index];
            if self.trigger_depth == 0 && bytes[index..].starts_with(self.delimiter.as_bytes()) {
                let end = index + self.delimiter.len();
                self.push(if self.delimiter == ";" { end } else { index });
                self.start = end;
                index = end;
            } else if byte.is_ascii_whitespace() {
                index += 1;
            } else if self.is_line_comment(index) {
                index = self.line_end(index);
            } else if bytes[index..].starts_with(b"/*") {
                self.has_text |= self.is_mysql()
                    && (bytes.get(index + 2) == Some(&b'!')
                        || bytes.get(index + 2..index + 4) == Some(b"M!"));
                index = self.block_comment_end(index)?;
            } else if matches!(byte, b'\'' | b'"' | b'`') {
                self.has_text = true;
                let backslash = byte != b'`'
                    && (self.is_mysql() || (byte == b'\'' && self.is_escape_string(index)));
                index = self.quote_end(index, byte, backslash)?;
            } else if byte == b'[' && self.engine == EngineKind::Sqlite {
                self.has_text = true;
                index = bytes[index + 1..]
                    .iter()
                    .position(|byte| *byte == b']')
                    .map(|offset| index + offset + 2)
                    .ok_or_else(|| syntax_error("Unterminated quoted identifier", index))?;
            } else if byte == b'$'
                && self.engine == EngineKind::Postgres
                && let Some(end) = self.dollar_quote_end(index)?
            {
                self.has_text = true;
                index = end;
            } else if is_word_start(byte) {
                let mut end = index + 1;
                while end < bytes.len()
                    && is_word_byte(bytes[end])
                    && !bytes[end..].starts_with(self.delimiter.as_bytes())
                {
                    end += 1;
                }
                let word = &self.sql[index..end];
                if !self.has_text && self.is_mysql() && word.eq_ignore_ascii_case("DELIMITER") {
                    index = self.set_delimiter(end)?;
                    self.start = index;
                } else {
                    self.record_word(word);
                    index = end;
                }
            } else {
                self.has_text = true;
                index += 1;
            }
        }

        self.push(bytes.len());
        if self.statements.is_empty() {
            return Err(DatabaseError::new(
                ErrorKind::EmptyStatement,
                "There is no statement to run",
            ));
        }

        Ok(self.statements)
    }

    fn push(&mut self, end: usize) {
        if self.has_text {
            self.statements.push(ScriptStatement {
                sql: self.sql[self.start..end].to_owned(),
                start: self.start,
            });
        }

        self.has_text = false;
        self.keywords.clear();
        self.trigger_depth = 0;
    }

    fn record_word(&mut self, word: &str) {
        self.has_text = true;
        if self.engine != EngineKind::Sqlite {
            return;
        }

        if self.keywords.len() < 3 {
            self.keywords.push(word.to_ascii_uppercase());
        }

        let is_trigger = self.keywords.first().is_some_and(|word| word == "CREATE")
            && (self.keywords.get(1).is_some_and(|word| word == "TRIGGER")
                || (self
                    .keywords
                    .get(1)
                    .is_some_and(|word| matches!(word.as_str(), "TEMP" | "TEMPORARY"))
                    && self.keywords.get(2).is_some_and(|word| word == "TRIGGER")));
        if !is_trigger {
            return;
        }

        // CASE expressions have their own END inside a trigger's BEGIN/END.
        if word.eq_ignore_ascii_case("BEGIN")
            || (self.trigger_depth > 0 && word.eq_ignore_ascii_case("CASE"))
        {
            self.trigger_depth += 1;
        } else if word.eq_ignore_ascii_case("END") {
            self.trigger_depth = self.trigger_depth.saturating_sub(1);
        }
    }

    fn is_line_comment(&self, index: usize) -> bool {
        let bytes = self.sql.as_bytes();
        (self.is_mysql() && bytes[index] == b'#')
            || (bytes[index..].starts_with(b"--")
                && (!self.is_mysql() || bytes.get(index + 2).is_none_or(u8::is_ascii_whitespace)))
    }

    fn line_end(&self, index: usize) -> usize {
        self.sql.as_bytes()[index..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(self.sql.len(), |offset| index + offset + 1)
    }

    fn block_comment_end(&self, start: usize) -> DbResult<usize> {
        let bytes = self.sql.as_bytes();
        let mut depth = 1;
        let mut index = start + 2;
        while index + 1 < bytes.len() {
            if bytes[index..].starts_with(b"*/") {
                depth -= 1;
                if depth == 0 {
                    return Ok(index + 2);
                }
                index += 2;
            } else if self.engine == EngineKind::Postgres && bytes[index..].starts_with(b"/*") {
                depth += 1;
                index += 2;
            } else {
                index += 1;
            }
        }

        Err(syntax_error("Unterminated block comment", start))
    }

    fn is_escape_string(&self, index: usize) -> bool {
        let bytes = self.sql.as_bytes();
        self.engine == EngineKind::Postgres
            && index > 0
            && matches!(bytes[index - 1], b'e' | b'E')
            && (index == 1 || !is_word_byte(bytes[index - 2]))
    }

    fn quote_end(&self, start: usize, quote: u8, backslash: bool) -> DbResult<usize> {
        let bytes = self.sql.as_bytes();
        let mut index = start + 1;
        while index < bytes.len() {
            if backslash && bytes[index] == b'\\' {
                index += 2;
            } else if bytes[index] == quote {
                if bytes.get(index + 1) != Some(&quote) {
                    return Ok(index + 1);
                }
                index += 2;
            } else {
                index += 1;
            }
        }

        Err(syntax_error("Unterminated SQL quote", start))
    }

    fn dollar_quote_end(&self, start: usize) -> DbResult<Option<usize>> {
        let bytes = self.sql.as_bytes();
        let mut end = start + 1;
        while end < bytes.len()
            && (is_word_start(bytes[end]) || (end > start + 1 && bytes[end].is_ascii_digit()))
        {
            end += 1;
        }
        if bytes.get(end) != Some(&b'$') {
            return Ok(None);
        }

        let delimiter = &self.sql[start..=end];
        self.sql[end + 1..]
            .find(delimiter)
            .map(|offset| Some(end + 1 + offset + delimiter.len()))
            .ok_or_else(|| syntax_error("Unterminated dollar-quoted SQL body", start))
    }

    fn set_delimiter(&mut self, start: usize) -> DbResult<usize> {
        let end = self.line_end(start);
        let delimiter = self.sql[start..end].trim();
        if delimiter.is_empty() || delimiter.chars().any(char::is_whitespace) {
            return Err(syntax_error(
                "DELIMITER requires one nonempty delimiter",
                start,
            ));
        }

        self.delimiter = delimiter.to_owned();
        Ok(end)
    }
}

fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 128
}

fn is_word_byte(byte: u8) -> bool {
    is_word_start(byte) || byte.is_ascii_digit() || byte == b'$'
}

fn syntax_error(message: &str, position: usize) -> DatabaseError {
    DatabaseError::new(ErrorKind::Syntax, message).with_position(position)
}
