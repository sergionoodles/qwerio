//! Mapping tokio-postgres errors to core errors.

use std::error::Error as _;

use qwerio_core::error::{DatabaseError, ErrorKind, redact_credentials};
use tokio_postgres::error::{ErrorPosition, SqlState};

const MULTIPLE_COMMANDS_MESSAGE: &str = "cannot insert multiple commands into a prepared statement";
const TLS_HANDSHAKE_MESSAGE: &str = "error performing TLS handshake";
const TLS_UNSUPPORTED_MESSAGE: &str = "server does not support TLS";

/// Maps an error; `sql` converts server-reported character positions into
/// byte offsets of the submitted text.
pub(crate) fn map_error(error: tokio_postgres::Error, sql: Option<&str>) -> DatabaseError {
    if let Some(database_error) = error.as_db_error() {
        let code = database_error.code();
        let message = database_error.message();
        let kind = if message.contains(MULTIPLE_COMMANDS_MESSAGE) {
            ErrorKind::MultipleStatements
        } else {
            kind_for(code, message)
        };

        let mut mapped = DatabaseError::new(kind, message).with_code(code.code());

        if kind == ErrorKind::MultipleStatements {
            mapped.message =
                "Only one statement can run at a time; nothing was executed".to_owned();
        }

        if let Some(detail) = database_error.detail() {
            mapped = mapped.with_detail(detail);
        }

        if let Some(hint) = database_error.hint() {
            mapped = mapped.with_hint(hint);
        }

        if let (Some(ErrorPosition::Original(position)), Some(sql)) =
            (database_error.position(), sql)
        {
            mapped = mapped.with_position(byte_offset(sql, *position));
        }

        return mapped;
    }

    if error.is_closed() {
        return DatabaseError::new(
            ErrorKind::Connection,
            "The connection to the server was lost",
        );
    }

    if error
        .source()
        .is_some_and(|cause| cause.to_string() == TLS_UNSUPPORTED_MESSAGE)
    {
        return DatabaseError::new(
            ErrorKind::Tls,
            "The server does not support TLS. To connect without encryption, set Encryption to \"No encryption\".",
        );
    }

    let kind = if is_tls_error(&error) {
        ErrorKind::Tls
    } else {
        ErrorKind::Connection
    };

    DatabaseError::new(kind, redact_credentials(&describe_chain(&error)))
}

fn kind_for(code: &SqlState, message: &str) -> ErrorKind {
    let code_text = code.code();

    match *code {
        SqlState::QUERY_CANCELED => ErrorKind::Cancelled,
        SqlState::INVALID_PASSWORD | SqlState::INVALID_AUTHORIZATION_SPECIFICATION => {
            ErrorKind::Authentication
        }
        SqlState::INSUFFICIENT_PRIVILEGE => ErrorKind::Permission,
        SqlState::SYNTAX_ERROR => ErrorKind::Syntax,
        SqlState::UNDEFINED_TABLE
        | SqlState::UNDEFINED_COLUMN
        | SqlState::UNDEFINED_FUNCTION
        | SqlState::UNDEFINED_OBJECT
        | SqlState::INVALID_SCHEMA_NAME => ErrorKind::UndefinedObject,
        SqlState::INVALID_CATALOG_NAME => ErrorKind::NotFound,
        SqlState::READ_ONLY_SQL_TRANSACTION => ErrorKind::ReadOnly,
        SqlState::IN_FAILED_SQL_TRANSACTION => ErrorKind::TransactionAborted,
        SqlState::LOCK_NOT_AVAILABLE | SqlState::T_R_DEADLOCK_DETECTED => ErrorKind::Busy,
        SqlState::PROTOCOL_VIOLATION if message.contains("parameters") => ErrorKind::Parameters,
        _ if code_text.starts_with("23") => ErrorKind::Constraint,
        _ if code_text.starts_with("08") => ErrorKind::Connection,
        _ => ErrorKind::Other,
    }
}

/// PostgreSQL reports 1-based character positions.
fn byte_offset(sql: &str, position: u32) -> usize {
    let index = usize::try_from(position.saturating_sub(1)).unwrap_or(usize::MAX);

    sql.char_indices()
        .nth(index)
        .map_or(sql.len(), |(offset, _)| offset)
}

fn describe_chain(error: &tokio_postgres::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();

    while let Some(cause) = source {
        let cause_text = cause.to_string();

        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }

        source = cause.source();
    }

    text
}

fn is_tls_error(error: &tokio_postgres::Error) -> bool {
    // tokio-postgres exposes no TLS-kind accessor, and negotiation failures
    // (such as a server rejecting TLS) need not contain a rustls error.
    if error.to_string() == TLS_HANDSHAKE_MESSAGE {
        return true;
    }

    let mut source = error.source();

    while let Some(cause) = source {
        if cause.downcast_ref::<rustls::Error>().is_some() {
            return true;
        }

        if let Some(io_error) = cause.downcast_ref::<std::io::Error>()
            && io_error
                .get_ref()
                .is_some_and(|inner| inner.downcast_ref::<rustls::Error>().is_some())
        {
            return true;
        }

        source = cause.source();
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_characters_not_bytes() {
        let sql = "SELECT 'é' FROM";

        assert_eq!(byte_offset(sql, 1), 0);
        assert_eq!(byte_offset(sql, 10), 10);
        assert_eq!(byte_offset(sql, 100), sql.len());
    }
}
