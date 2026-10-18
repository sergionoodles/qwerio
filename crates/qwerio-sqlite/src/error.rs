//! Mapping rusqlite errors to core errors.

use qwerio_core::error::{DatabaseError, ErrorKind};
use rusqlite::ErrorCode;

pub(crate) fn map_error(error: rusqlite::Error) -> DatabaseError {
    match error {
        rusqlite::Error::SqliteFailure(failure, message) => {
            let message = message.unwrap_or_else(|| failure.to_string());
            DatabaseError::new(kind_for(failure.code, &message), message)
                .with_code(format!("SQLITE {}", failure.extended_code))
        }
        rusqlite::Error::SqlInputError {
            error, msg, offset, ..
        } => {
            let error = DatabaseError::new(kind_for(error.code, &msg), msg)
                .with_code(format!("SQLITE {}", error.extended_code));

            match usize::try_from(offset) {
                Ok(offset) => error.with_position(offset),
                Err(_) => error,
            }
        }
        rusqlite::Error::MultipleStatement => DatabaseError::new(
            ErrorKind::MultipleStatements,
            "Only one statement can run at a time; nothing was executed",
        ),
        rusqlite::Error::InvalidParameterCount(..) | rusqlite::Error::InvalidParameterName(_) => {
            DatabaseError::new(ErrorKind::Parameters, error.to_string())
        }
        rusqlite::Error::ToSqlConversionFailure(source) => {
            DatabaseError::new(ErrorKind::Parameters, source.to_string())
        }
        other => DatabaseError::new(ErrorKind::Other, other.to_string()),
    }
}

fn kind_for(code: ErrorCode, message: &str) -> ErrorKind {
    match code {
        ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => ErrorKind::Busy,
        ErrorCode::ReadOnly => ErrorKind::ReadOnly,
        ErrorCode::PermissionDenied | ErrorCode::AuthorizationForStatementDenied => {
            ErrorKind::Permission
        }
        ErrorCode::OperationInterrupted => ErrorKind::Cancelled,
        ErrorCode::ConstraintViolation => ErrorKind::Constraint,
        ErrorCode::CannotOpen | ErrorCode::NotADatabase | ErrorCode::SystemIoFailure => {
            ErrorKind::Connection
        }
        _ if message.contains("syntax error") || message.starts_with("incomplete input") => {
            ErrorKind::Syntax
        }
        _ if message.starts_with("no such ") => ErrorKind::UndefinedObject,
        _ => ErrorKind::Other,
    }
}
