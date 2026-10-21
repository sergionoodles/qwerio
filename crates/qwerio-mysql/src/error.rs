//! Normalize diagnostics without echoing connection credentials.

use mysql_async::Error;
use qwerio_core::error::{DatabaseError, ErrorKind, redact_credentials};

pub(crate) fn map_error(error: Error) -> DatabaseError {
    match error {
        Error::Server(error) => {
            let kind = match error.code {
                1044 | 1142 | 1143 | 1227 => ErrorKind::Permission,
                1045 | 1698 => ErrorKind::Authentication,
                1049 | 1051 | 1054 | 1146 => ErrorKind::UndefinedObject,
                1062 | 1048 | 1216 | 1217 | 1451 | 1452 | 3819 | 4025 => ErrorKind::Constraint,
                1064 | 1149 => ErrorKind::Syntax,
                1205 | 1213 | 3572 => ErrorKind::Busy,
                1290 | 1792 => ErrorKind::ReadOnly,
                1317 | 3024 => ErrorKind::Cancelled,
                1295 => ErrorKind::Unsupported,
                _ => ErrorKind::Other,
            };
            DatabaseError::new(kind, redact_credentials(&error.message))
                .with_code(format!("{} ({})", error.code, error.state))
        }
        Error::Io(mysql_async::IoError::Tls(error)) => {
            DatabaseError::new(ErrorKind::Tls, redact_credentials(&error.to_string()))
        }
        Error::Io(error) => {
            // Tokio-rustls also wraps certificate failures in io::Error.
            let message = redact_credentials(&error.to_string());
            let kind = if message.to_ascii_lowercase().contains("certificate") {
                ErrorKind::Tls
            } else {
                ErrorKind::Connection
            };
            DatabaseError::new(kind, message)
        }
        other => {
            let message = redact_credentials(&other.to_string());
            let kind = if message.to_ascii_lowercase().contains("tls")
                || message.to_ascii_lowercase().contains("certificate")
            {
                ErrorKind::Tls
            } else {
                ErrorKind::Connection
            };
            DatabaseError::new(kind, message)
        }
    }
}
