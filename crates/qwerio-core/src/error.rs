//! Engine-neutral database errors.

use std::fmt;

use serde::{Deserialize, Serialize};

pub type DbResult<T> = Result<T, DatabaseError>;

/// Broad error categories the UI can react to without knowing the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorKind {
    /// The connection could not be established or was lost.
    Connection,
    Authentication,
    /// TLS negotiation or certificate validation failed.
    Tls,
    /// The role or file permissions do not allow the operation.
    Permission,
    Syntax,
    /// A referenced table, column, or other object does not exist.
    UndefinedObject,
    Constraint,
    /// The database or file is locked by another connection.
    Busy,
    /// A write was attempted on a read-only session.
    ReadOnly,
    Cancelled,
    /// More than one statement was submitted where exactly one is allowed.
    MultipleStatements,
    /// The submitted text contains no statement.
    EmptyStatement,
    /// Parameter placeholders without values, or invalid parameter values.
    Parameters,
    /// The current transaction failed; only ROLLBACK can proceed.
    TransactionAborted,
    /// The request is invalid before reaching the database.
    InvalidRequest,
    /// A paging token does not match the current query or metadata.
    InvalidContinuation,
    /// The session already runs a request.
    SessionBusy,
    SessionClosed,
    NotFound,
    Unsupported,
    Internal,
    /// Any other engine error; see the message and code.
    Other,
}

/// A position inside submitted SQL, as a byte offset into the request text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePosition {
    pub byte_offset: usize,
}

/// An error reported by an adapter or by request validation.
///
/// Messages must never include credentials. Adapters build errors from engine
/// diagnostics, which do not contain passwords, and redact connection strings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseError {
    pub kind: ErrorKind,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    /// Engine-specific code such as a PostgreSQL SQLSTATE or SQLite result code.
    pub code: Option<String>,
    pub position: Option<SourcePosition>,
}

impl DatabaseError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: None,
            hint: None,
            code: None,
            position: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn with_position(mut self, byte_offset: usize) -> Self {
        self.position = Some(SourcePosition { byte_offset });
        self
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidRequest, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, "The request was cancelled")
    }

    pub fn session_closed() -> Self {
        Self::new(ErrorKind::SessionClosed, "The session is closed")
    }

    pub fn session_busy() -> Self {
        Self::new(
            ErrorKind::SessionBusy,
            "The session is already running a request",
        )
    }
}

impl fmt::Display for DatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)?;

        if let Some(code) = &self.code {
            write!(formatter, " ({code})")?;
        }

        Ok(())
    }
}

impl std::error::Error for DatabaseError {}

/// Removes `password=...` pairs and URL user-info passwords from text that may
/// echo a connection string, such as driver configuration errors.
pub fn redact_credentials(text: &str) -> String {
    let without_url_passwords = redact_url_passwords(text);
    redact_key_value_passwords(&without_url_passwords)
}

const REDACTED: &str = "********";

fn redact_url_passwords(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(scheme_end) = rest.find("://") {
        let authority_start = scheme_end + 3;
        output.push_str(&rest[..authority_start]);
        rest = &rest[authority_start..];

        let authority_end = rest
            .find(|character: char| character == '/' || character.is_whitespace())
            .unwrap_or(rest.len());
        let authority = &rest[..authority_end];

        match (authority.rfind('@'), authority.find(':')) {
            (Some(at), Some(colon)) if colon < at => {
                output.push_str(&authority[..=colon]);
                output.push_str(REDACTED);
                output.push_str(&authority[at..]);
            }
            _ => output.push_str(authority),
        }

        rest = &rest[authority_end..];
    }

    output.push_str(rest);
    output
}

fn redact_key_value_passwords(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    let key = "password=";

    while let Some(start) = find_ascii_case_insensitive(rest, key) {
        let value_start = start + key.len();
        output.push_str(&rest[..value_start]);
        rest = &rest[value_start..];

        let value_end = password_value_end(rest);
        output.push_str(REDACTED);
        rest = &rest[value_end..];
    }

    output.push_str(rest);
    output
}

fn password_value_end(text: &str) -> usize {
    if let Some(quoted) = text.strip_prefix('\'') {
        let mut escaped = false;

        for (index, character) in quoted.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }

            match character {
                '\\' => escaped = true,
                '\'' => return index + 2,
                _ => {}
            }
        }

        return text.len();
    }

    text.find(|character: char| character.is_whitespace() || character == '&')
        .unwrap_or(text.len())
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_url_passwords() {
        let text = "failed to connect to postgres://alice:s3cret@db.example/app";

        assert_eq!(
            redact_credentials(text),
            "failed to connect to postgres://alice:********@db.example/app"
        );
    }

    #[test]
    fn redacts_key_value_passwords_including_quoted_values() {
        let text = "host=db password='a b\\' c' user=x PASSWORD=plain";

        assert_eq!(
            redact_credentials(text),
            "host=db password=******** user=x PASSWORD=********"
        );
    }

    #[test]
    fn leaves_text_without_credentials_unchanged() {
        let text = "relation \"users\" does not exist";

        assert_eq!(redact_credentials(text), text);
    }
}
