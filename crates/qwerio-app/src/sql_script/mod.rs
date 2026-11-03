//! Sequential SQL execution plans. Every statement retains its document offset
//! and uses the console's existing session, transaction, and result budget.

mod boundary;
mod mysql;
mod split;

pub use mysql::MysqlSqlMode;

use std::collections::VecDeque;

use qwerio_core::connection::EngineKind;
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};

use crate::console::RunOutcome;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptStatement {
    pub sql: String,
    pub start: usize,
}

pub struct SqlRun {
    statements: VecDeque<ScriptStatement>,
    mysql_script: Option<mysql::MysqlScript>,
    total: Option<usize>,
    started: usize,
    completed: usize,
    is_active: bool,
    is_stopped: bool,
    is_script: bool,
}

impl SqlRun {
    pub fn statement(sql: String, start: usize) -> Self {
        Self::new(vec![ScriptStatement { sql, start }], false)
    }

    /// Validates fixed-dialect scripts up front. MySQL and MariaDB are split
    /// as they advance because statements can change the session's SQL mode.
    pub fn script(sql: &str, engine: EngineKind) -> DbResult<Self> {
        if matches!(engine, EngineKind::Mysql | EngineKind::Mariadb) {
            return Ok(Self {
                mysql_script: Some(mysql::MysqlScript::new(sql, engine)),
                total: None,
                ..Self::new(Vec::new(), true)
            });
        }
        split::statements(sql, engine, MysqlSqlMode::default())
            .map(|statements| Self::new(statements, true))
    }

    fn new(statements: Vec<ScriptStatement>, is_script: bool) -> Self {
        Self {
            total: Some(statements.len()),
            statements: statements.into(),
            mysql_script: None,
            started: 0,
            completed: 0,
            is_active: false,
            is_stopped: false,
            is_script,
        }
    }

    /// The next statement is available only after the previous request's
    /// Finished event, when the session can accept another request.
    pub fn next_statement(&mut self, mode: MysqlSqlMode) -> DbResult<Option<ScriptStatement>> {
        if self.is_active || self.is_stopped {
            return Ok(None);
        }

        let statement = match &mut self.mysql_script {
            Some(script) => script.next_statement(mode)?,
            None => self.statements.pop_front(),
        };
        let Some(statement) = statement else {
            self.total = Some(self.started);
            if self.started == 0 && self.is_script {
                return Err(DatabaseError::new(
                    ErrorKind::EmptyStatement,
                    "There is no statement to run",
                ));
            }
            return Ok(None);
        };
        self.started += 1;
        self.is_active = true;
        Ok(Some(statement))
    }

    pub fn complete_statement(&mut self, outcome: &RunOutcome) {
        if !self.is_active {
            return;
        }

        self.is_active = false;
        if *outcome == RunOutcome::Succeeded {
            self.completed += 1;
        } else {
            self.stop();
        }
    }

    /// Also prevents later statements when cancellation races with a
    /// successful completion of the current statement.
    pub fn stop(&mut self) {
        self.is_stopped = true;
    }

    pub fn is_script(&self) -> bool {
        self.is_script
    }

    /// MySQL's total is known only after its final statement is reached.
    pub fn total(&self) -> Option<usize> {
        self.total
    }

    pub fn started(&self) -> usize {
        self.started
    }

    pub fn completed(&self) -> usize {
        self.completed
    }
}

#[cfg(test)]
mod tests;
