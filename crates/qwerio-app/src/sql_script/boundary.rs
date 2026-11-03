use qwerio_core::connection::EngineKind;

const ROUTINE_PREFIX_WORDS: usize = 4;

pub(super) struct StatementBoundary {
    engine: EngineKind,
    sqlite: SqliteBoundary,
    postgres: PostgresBoundary,
}

impl StatementBoundary {
    pub(super) fn new(engine: EngineKind) -> Self {
        Self {
            engine,
            sqlite: SqliteBoundary::Start,
            postgres: PostgresBoundary::default(),
        }
    }

    pub(super) fn record_word(&mut self, word: &str) {
        match self.engine {
            EngineKind::Sqlite => self.sqlite.record_word(word),
            EngineKind::Postgres => self.postgres.record_word(word),
            _ => {}
        }
    }

    pub(super) fn record_other(&mut self, byte: u8) {
        match self.engine {
            EngineKind::Sqlite => self.sqlite.record_other(),
            EngineKind::Postgres => {
                self.postgres.after_dot = byte == b'.';
                self.postgres.after_begin = false;
                self.postgres.after_semicolon = false;
            }
            _ => {}
        }
    }

    pub(super) fn record_delimiter(&mut self) -> bool {
        match self.engine {
            EngineKind::Sqlite => self.sqlite.record_delimiter(),
            EngineKind::Postgres => {
                self.postgres.after_semicolon = true;
                self.postgres.after_begin = false;
                self.postgres.bodies.is_empty()
            }
            _ => true,
        }
    }
}

enum SqliteBoundary {
    Start,
    Explain,
    Query,
    Plan,
    Create,
    Temporary,
    Ordinary,
    Trigger,
    TriggerSemicolon,
    TriggerEnd,
}

impl SqliteBoundary {
    fn record_word(&mut self, word: &str) {
        *self = match (&self, word.to_ascii_uppercase().as_str()) {
            (Self::Start, "EXPLAIN") => Self::Explain,
            (Self::Explain, "QUERY") => Self::Query,
            (Self::Query, "PLAN") => Self::Plan,
            (Self::Start | Self::Explain | Self::Plan, "CREATE") => Self::Create,
            (Self::Create, "TEMP" | "TEMPORARY") => Self::Temporary,
            (Self::Create | Self::Temporary, "TRIGGER") => Self::Trigger,
            (Self::TriggerSemicolon, "END") => Self::TriggerEnd,
            (Self::Trigger | Self::TriggerSemicolon | Self::TriggerEnd, _) => Self::Trigger,
            _ => Self::Ordinary,
        };
    }

    fn record_other(&mut self) {
        *self = match self {
            Self::Trigger | Self::TriggerSemicolon | Self::TriggerEnd => Self::Trigger,
            _ => Self::Ordinary,
        };
    }

    fn record_delimiter(&mut self) -> bool {
        // SQLite trigger bodies end at "; END;". BEGIN, CASE and END may
        // otherwise be identifiers, so counting those words is insufficient.
        match self {
            Self::Trigger | Self::TriggerSemicolon => {
                *self = Self::TriggerSemicolon;
                false
            }
            _ => true,
        }
    }
}

#[derive(Default)]
struct PostgresBoundary {
    keywords: Vec<String>,
    bodies: Vec<PostgresBody>,
    after_begin: bool,
    after_semicolon: bool,
    after_dot: bool,
}

enum PostgresBody {
    Atomic,
    Case,
}

impl PostgresBoundary {
    fn record_word(&mut self, word: &str) {
        let word = word.to_ascii_uppercase();
        if self.keywords.len() < ROUTINE_PREFIX_WORDS {
            self.keywords.push(word.clone());
        }

        let is_routine = matches!(self.keywords.as_slice(), [create, kind, ..]
            if create == "CREATE" && matches!(kind.as_str(), "FUNCTION" | "PROCEDURE"))
            || matches!(self.keywords.as_slice(), [create, or, replace, kind]
                if create == "CREATE" && or == "OR" && replace == "REPLACE"
                    && matches!(kind.as_str(), "FUNCTION" | "PROCEDURE"));
        if !self.after_dot && is_routine {
            match word.as_str() {
                "ATOMIC" if self.after_begin => self.bodies.push(PostgresBody::Atomic),
                "CASE" if !self.bodies.is_empty() => self.bodies.push(PostgresBody::Case),
                "END"
                    if matches!(self.bodies.last(), Some(PostgresBody::Case))
                        || self.after_semicolon =>
                {
                    self.bodies.pop();
                }
                _ => {}
            }
        }

        self.after_begin = word == "BEGIN" && !self.after_dot;
        self.after_dot = false;
        self.after_semicolon = false;
    }
}
