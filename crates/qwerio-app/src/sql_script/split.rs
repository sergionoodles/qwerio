use qwerio_core::connection::EngineKind;
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};

use super::boundary::StatementBoundary;
use super::{MysqlSqlMode, ScriptStatement};

pub(super) fn statements(
    sql: &str,
    engine: EngineKind,
    mode: MysqlSqlMode,
) -> DbResult<Vec<ScriptStatement>> {
    let mut splitter = Splitter::new(sql, engine, mode, ";");
    let mut statements = Vec::new();
    while let Some(statement) = splitter.next_statement()? {
        statements.push(statement);
    }
    if statements.is_empty() {
        return Err(DatabaseError::new(
            ErrorKind::EmptyStatement,
            "There is no statement to run",
        ));
    }

    Ok(statements)
}

pub(super) struct NextStatement {
    pub statement: Option<ScriptStatement>,
    pub consumed: usize,
    pub delimiter: String,
}

pub(super) fn next_mysql_statement(
    sql: &str,
    engine: EngineKind,
    mode: MysqlSqlMode,
    delimiter: &str,
) -> DbResult<NextStatement> {
    let mut splitter = Splitter::new(sql, engine, mode, delimiter);
    let statement = splitter.next_statement()?;
    Ok(NextStatement {
        statement,
        consumed: splitter.index,
        delimiter: splitter.delimiter,
    })
}

struct Splitter<'a> {
    sql: &'a str,
    engine: EngineKind,
    mode: MysqlSqlMode,
    delimiter: String,
    index: usize,
    start: usize,
    has_text: bool,
    boundary: StatementBoundary,
}

impl<'a> Splitter<'a> {
    fn new(sql: &'a str, engine: EngineKind, mode: MysqlSqlMode, delimiter: &str) -> Self {
        Self {
            sql,
            engine,
            mode,
            delimiter: delimiter.to_owned(),
            index: 0,
            start: 0,
            has_text: false,
            boundary: StatementBoundary::new(engine),
        }
    }

    fn is_mysql(&self) -> bool {
        matches!(self.engine, EngineKind::Mysql | EngineKind::Mariadb)
    }

    fn next_statement(&mut self) -> DbResult<Option<ScriptStatement>> {
        let bytes = self.sql.as_bytes();

        while self.index < bytes.len() {
            let index = self.index;
            let byte = bytes[index];
            if bytes[index..].starts_with(self.delimiter.as_bytes()) {
                let end = index + self.delimiter.len();
                self.index = end;
                if self.boundary.record_delimiter() {
                    let statement =
                        self.take_statement(if self.delimiter == ";" { end } else { index });
                    self.start = end;
                    if statement.is_some() {
                        return Ok(statement);
                    }
                }
            } else if byte.is_ascii_whitespace() {
                self.index += 1;
            } else if self.is_line_comment(index) {
                self.index = self.line_end(index);
            } else if bytes[index..].starts_with(b"/*") {
                self.has_text |= self.is_mysql()
                    && (bytes.get(index + 2) == Some(&b'!')
                        || bytes.get(index + 2..index + 4) == Some(b"M!"));
                self.index = self.block_comment_end(index)?;
            } else if matches!(byte, b'\'' | b'"')
                || (byte == b'`' && self.engine != EngineKind::Postgres)
            {
                self.record_other(byte);
                let backslash = byte != b'`'
                    && ((self.is_mysql() && !self.mode.no_backslash_escapes)
                        || (byte == b'\'' && self.is_escape_string(index)));
                self.index = self.quote_end(index, byte, backslash)?;
            } else if byte == b'[' && self.engine == EngineKind::Sqlite {
                self.record_other(byte);
                self.index = bytes[index + 1..]
                    .iter()
                    .position(|byte| *byte == b']')
                    .map(|offset| index + offset + 2)
                    .ok_or_else(|| syntax_error("Unterminated quoted identifier", index))?;
            } else if byte == b'$'
                && self.engine == EngineKind::Postgres
                && let Some(end) = self.dollar_quote_end(index)?
            {
                self.record_other(byte);
                self.index = end;
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
                    self.index = self.set_delimiter(end)?;
                    self.start = self.index;
                } else {
                    self.has_text = true;
                    self.boundary.record_word(word);
                    self.index = end;
                }
            } else {
                self.record_other(byte);
                self.index += 1;
            }
        }

        Ok(self.take_statement(bytes.len()))
    }

    fn take_statement(&mut self, end: usize) -> Option<ScriptStatement> {
        let statement = self.has_text.then(|| ScriptStatement {
            sql: self.sql[self.start..end].to_owned(),
            start: self.start,
        });

        self.has_text = false;
        self.boundary = StatementBoundary::new(self.engine);
        statement
    }

    fn record_other(&mut self, byte: u8) {
        self.has_text = true;
        self.boundary.record_other(byte);
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
