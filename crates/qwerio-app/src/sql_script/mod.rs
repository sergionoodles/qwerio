//! Sequential SQL execution plans. Every statement retains its document offset
//! and uses the console's existing session, transaction, and result budget.

mod split;

use std::collections::VecDeque;

use qwerio_core::connection::EngineKind;
use qwerio_core::error::DbResult;

use crate::console::RunOutcome;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptStatement {
    pub sql: String,
    pub start: usize,
}

pub struct SqlRun {
    statements: VecDeque<ScriptStatement>,
    total: usize,
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

    /// Splits the whole document before executing any of it. Client delimiter
    /// directives are supported for MySQL and MariaDB.
    pub fn script(sql: &str, engine: EngineKind) -> DbResult<Self> {
        split::statements(sql, engine).map(|statements| Self::new(statements, true))
    }

    fn new(statements: Vec<ScriptStatement>, is_script: bool) -> Self {
        Self {
            total: statements.len(),
            statements: statements.into(),
            started: 0,
            completed: 0,
            is_active: false,
            is_stopped: false,
            is_script,
        }
    }

    /// The next statement is available only after the previous request's
    /// Finished event, when the session can accept another request.
    pub fn next_statement(&mut self) -> Option<ScriptStatement> {
        if self.is_active || self.is_stopped {
            return None;
        }

        let statement = self.statements.pop_front()?;
        self.started += 1;
        self.is_active = true;
        Some(statement)
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

    pub fn total(&self) -> usize {
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
