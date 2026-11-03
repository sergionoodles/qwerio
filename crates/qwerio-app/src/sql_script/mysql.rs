use qwerio_core::connection::EngineKind;
use qwerio_core::error::DbResult;

use super::{ScriptStatement, split};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MysqlSqlMode {
    pub no_backslash_escapes: bool,
}

pub(super) struct MysqlScript {
    sql: String,
    engine: EngineKind,
    offset: usize,
    delimiter: String,
}

impl MysqlScript {
    pub(super) fn new(sql: &str, engine: EngineKind) -> Self {
        Self {
            sql: sql.to_owned(),
            engine,
            offset: 0,
            delimiter: ";".to_owned(),
        }
    }

    pub(super) fn next_statement(
        &mut self,
        mode: MysqlSqlMode,
    ) -> DbResult<Option<ScriptStatement>> {
        let next = split::next_mysql_statement(
            &self.sql[self.offset..],
            self.engine,
            mode,
            &self.delimiter,
        )
        .map_err(|error| match error.position {
            Some(position) => error.with_position(self.offset + position.byte_offset),
            None => error,
        })?;
        let statement = next.statement.map(|statement| ScriptStatement {
            start: self.offset + statement.start,
            ..statement
        });
        self.offset += next.consumed;
        self.delimiter = next.delimiter;
        Ok(statement)
    }
}
