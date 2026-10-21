//! Parsing pasted PostgreSQL, MySQL, and MariaDB URLs, such as a `DATABASE_URL`
//! value, into connection options.
//!
//! Accepts `postgres://` and `postgresql://` URLs in the libpq format. A
//! leading `jdbc:` prefix or a whole `.env` line (`DATABASE_URL="…"`) is
//! tolerated so values can be pasted as they are found.

use std::fmt;
use std::path::PathBuf;

use crate::connection::{DEFAULT_POSTGRES_PORT, PostgresOptions, Secret, TlsMode};

const POSTGRES_SCHEMES: [&str; 2] = ["postgres", "postgresql"];
const JDBC_PREFIX: &str = "jdbc:";
const EXPORT_PREFIX: &str = "export ";
const QUOTES: [char; 2] = ['"', '\''];
const DEFAULT_POSTGRES_HOST: &str = "localhost";
const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "::1", "127.0.0.1"];
/// libpq's `sslrootcert=system` means the platform trust store.
const SYSTEM_ROOT_CERT: &str = "system";

/// Options read from a connection string, with its password kept apart.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedConnectionString {
    pub options: PostgresOptions,
    pub password: Option<Secret>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionStringError {
    MissingScheme,
    UnsupportedScheme(String),
    UnsupportedMysqlScheme(String),
    InvalidPort(String),
    InvalidEscape,
    UnsupportedSslMode(String),
}

impl fmt::Display for ConnectionStringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingScheme => {
                formatter.write_str("Enter a URL such as postgres://user@host/database")
            }
            Self::UnsupportedScheme(scheme) => write!(
                formatter,
                "\"{scheme}\" connection strings are not supported. Use postgres://"
            ),
            Self::UnsupportedMysqlScheme(scheme) => write!(
                formatter,
                "\"{scheme}\" connection strings are not supported here. Use mysql:// or mariadb://"
            ),
            Self::InvalidPort(port) => write!(formatter, "\"{port}\" is not a valid port"),
            Self::InvalidEscape => {
                formatter.write_str("The connection string contains an invalid %-escape")
            }
            Self::UnsupportedSslMode(mode) => write!(formatter, "Unsupported sslmode \"{mode}\""),
        }
    }
}

impl std::error::Error for ConnectionStringError {}

pub fn parse_connection_string(
    input: &str,
) -> Result<ParsedConnectionString, ConnectionStringError> {
    let url = strip_wrapping(input);
    let url = url.strip_prefix(JDBC_PREFIX).unwrap_or(url);
    let (scheme, rest) = url
        .split_once(':')
        .ok_or(ConnectionStringError::MissingScheme)?;
    let scheme = scheme.to_ascii_lowercase();

    if POSTGRES_SCHEMES.contains(&scheme.as_str()) {
        return parse_postgres(rest);
    }

    Err(ConnectionStringError::UnsupportedScheme(scheme))
}

/// MySQL/MariaDB endpoint parsed separately to preserve PostgreSQL URL semantics.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedMysqlConnectionString {
    pub options: crate::connection::MysqlOptions,
    pub password: Option<Secret>,
}

pub fn parse_mysql_connection_string(
    input: &str,
) -> Result<ParsedMysqlConnectionString, ConnectionStringError> {
    let url = strip_wrapping(input);
    let url = url.strip_prefix(JDBC_PREFIX).unwrap_or(url);
    let (scheme, rest) = url
        .split_once(':')
        .ok_or(ConnectionStringError::MissingScheme)?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "mysql" | "mariadb") {
        return Err(ConnectionStringError::UnsupportedMysqlScheme(
            scheme.to_owned(),
        ));
    }
    let mut parts = parse_parts(rest)?;
    let query = rest.split_once('?').map_or("", |(_, query)| query);
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key)?.to_ascii_lowercase();
        let value = percent_decode(value)?;
        match key.as_str() {
            "ssl-mode" | "sslmode" => {
                parts.sslmode = Some(match value.to_ascii_lowercase().as_str() {
                    "disabled" | "disable" => "disable".to_owned(),
                    "required" | "require" | "verify_ca" | "verify_identity" | "verify-full"
                    | "verify-ca" => "verify-full".to_owned(),
                    _ => return Err(ConnectionStringError::UnsupportedSslMode(value)),
                })
            }
            "require_ssl" if value == "true" => parts.sslmode = Some("verify-full".to_owned()),
            "verify_ca" | "verify_identity" if value != "true" => {
                return Err(ConnectionStringError::UnsupportedSslMode(
                    "Certificate verification cannot be disabled".to_owned(),
                ));
            }
            "ssl-ca" => parts.sslrootcert = non_empty(value),
            _ => {}
        }
    }
    let host = parts
        .host
        .unwrap_or_else(|| DEFAULT_POSTGRES_HOST.to_owned());
    let port = parts
        .port
        .map(|port| {
            port.parse::<u16>()
                .map_err(|_| ConnectionStringError::InvalidPort(port))
        })
        .transpose()?
        .unwrap_or(crate::connection::DEFAULT_MYSQL_PORT);
    if port == 0 {
        return Err(ConnectionStringError::InvalidPort("0".to_owned()));
    }
    let tls = tls_mode(
        parts.sslmode.as_deref(),
        parts.sslrootcert.map(PathBuf::from),
        &host,
    )?;
    Ok(ParsedMysqlConnectionString {
        options: crate::connection::MysqlOptions {
            host,
            port,
            user: parts.user.unwrap_or_default(),
            database: parts.database.unwrap_or_default(),
            show_all_databases: true,
            tls,
            connect_timeout: None,
        },
        password: parts.password.map(Secret::new),
    })
}

/// Removes surrounding whitespace, quotes, and an `.env` style `NAME=` prefix.
fn strip_wrapping(input: &str) -> &str {
    let trimmed = input.trim();
    let trimmed = trimmed.strip_prefix(EXPORT_PREFIX).unwrap_or(trimmed);
    let value = match trimmed.split_once('=') {
        Some((name, value)) if is_variable_name(name.trim()) => value.trim(),
        _ => trimmed,
    };

    QUOTES
        .iter()
        .find_map(|quote| {
            value
                .strip_prefix(*quote)
                .and_then(|inner| inner.strip_suffix(*quote))
        })
        .unwrap_or(value)
}

fn is_variable_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Fields collected from the URL, before libpq's defaults are applied.
#[derive(Default)]
struct PostgresParts {
    host: Option<String>,
    port: Option<String>,
    user: Option<String>,
    password: Option<String>,
    database: Option<String>,
    sslmode: Option<String>,
    sslrootcert: Option<String>,
}

fn parse_postgres(rest: &str) -> Result<ParsedConnectionString, ConnectionStringError> {
    build_postgres(parse_parts(rest)?)
}

fn parse_parts(rest: &str) -> Result<PostgresParts, ConnectionStringError> {
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let rest = rest.split_once('#').map_or(rest, |(before, _)| before);
    let (location, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, path) = location.split_once('/').unwrap_or((location, ""));
    // The password may contain an unescaped `@`; the host never does.
    let (userinfo, hosts) = match authority.rsplit_once('@') {
        Some((userinfo, hosts)) => (Some(userinfo), hosts),
        None => (None, authority),
    };

    let mut parts = PostgresParts::default();

    if let Some(userinfo) = userinfo {
        let (user, password) = match userinfo.split_once(':') {
            Some((user, password)) => (user, Some(password)),
            None => (userinfo, None),
        };
        parts.user = non_empty(percent_decode(user)?);
        parts.password = password.map(percent_decode).transpose()?;
    }

    // libpq accepts several fallback hosts; the form holds one.
    let first_host = hosts.split(',').next().unwrap_or_default();
    let (host, port) = split_host_port(first_host);
    parts.host = non_empty(percent_decode(host)?);
    parts.port = port.map(str::to_owned);
    parts.database = non_empty(percent_decode(path)?);

    apply_query(&mut parts, query)?;
    Ok(parts)
}

/// Splits `host:port`, keeping the colons of a bracketed IPv6 address.
fn split_host_port(text: &str) -> (&str, Option<&str>) {
    if let Some(bracketed) = text.strip_prefix('[') {
        let (address, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
        return (address, after.strip_prefix(':'));
    }

    match text.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (text, None),
    }
}

/// Query parameters override the URL's own parts, as in libpq.
fn apply_query(parts: &mut PostgresParts, query: &str) -> Result<(), ConnectionStringError> {
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = percent_decode(value)?;

        let slot = match percent_decode(key)?.as_str() {
            "host" => &mut parts.host,
            "port" => &mut parts.port,
            "user" => &mut parts.user,
            "password" => &mut parts.password,
            "dbname" => &mut parts.database,
            "sslmode" => &mut parts.sslmode,
            "sslrootcert" => &mut parts.sslrootcert,
            // Pooler and ORM settings such as `schema` or `pgbouncer` do not
            // map onto the form.
            _ => continue,
        };
        *slot = non_empty(value);
    }

    Ok(())
}

fn build_postgres(parts: PostgresParts) -> Result<ParsedConnectionString, ConnectionStringError> {
    let host = parts
        .host
        .unwrap_or_else(|| DEFAULT_POSTGRES_HOST.to_owned());
    let port = match parts.port.filter(|port| !port.is_empty()) {
        Some(port) => port
            .parse::<u16>()
            .map_err(|_| ConnectionStringError::InvalidPort(port))?,
        None => DEFAULT_POSTGRES_PORT,
    };
    let user = parts.user.unwrap_or_default();
    // libpq opens the database named after the user when none is given.
    let database = parts.database.unwrap_or_else(|| user.clone());
    let ca_file = parts
        .sslrootcert
        .filter(|path| path != SYSTEM_ROOT_CERT)
        .map(PathBuf::from);
    let tls = tls_mode(parts.sslmode.as_deref(), ca_file, &host)?;

    Ok(ParsedConnectionString {
        options: PostgresOptions {
            host,
            port,
            user,
            database,
            show_all_databases: true,
            tls,
            connect_timeout: None,
            application_name: None,
        },
        password: parts.password.map(Secret::new),
    })
}

/// Qwerio either verifies the certificate or does not encrypt, so every
/// encrypting `sslmode` verifies. Without one, libpq's default `prefer` is
/// read as plain text for local servers and verified TLS for remote ones.
fn tls_mode(
    sslmode: Option<&str>,
    ca_file: Option<PathBuf>,
    host: &str,
) -> Result<TlsMode, ConnectionStringError> {
    match sslmode {
        Some("disable" | "allow") => Ok(TlsMode::Disable),
        Some("require" | "verify-ca" | "verify-full") => Ok(TlsMode::VerifyFull { ca_file }),
        None | Some("prefer") if ca_file.is_none() && is_local_host(host) => Ok(TlsMode::Disable),
        None | Some("prefer") => Ok(TlsMode::VerifyFull { ca_file }),
        Some(other) => Err(ConnectionStringError::UnsupportedSslMode(other.to_owned())),
    }
}

fn is_local_host(host: &str) -> bool {
    host.starts_with('/') || host.starts_with("127.") || LOOPBACK_HOSTS.contains(&host)
}

fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

fn percent_decode(text: &str) -> Result<String, ConnectionStringError> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while let Some(&byte) = bytes.get(index) {
        if byte != b'%' {
            decoded.push(byte);
            index += 1;
            continue;
        }

        let hex = bytes
            .get(index + 1..index + 3)
            .filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))
            .ok_or(ConnectionStringError::InvalidEscape)?;
        let hex = std::str::from_utf8(hex).map_err(|_| ConnectionStringError::InvalidEscape)?;
        let value =
            u8::from_str_radix(hex, 16).map_err(|_| ConnectionStringError::InvalidEscape)?;
        decoded.push(value);
        index += 3;
    }

    String::from_utf8(decoded).map_err(|_| ConnectionStringError::InvalidEscape)
}

#[cfg(test)]
mod tests;
