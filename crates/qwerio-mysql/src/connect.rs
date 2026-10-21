//! Connection setup shared by browsing, consoles, and cancellation.

use crate::error::map_error;
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, OptsBuilder, SslOpts};
use qwerio_core::connection::{Credentials, MysqlOptions, TlsMode};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::session::AccessMode;
use std::time::Duration;

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn options(options: &MysqlOptions, credentials: &Credentials, database: &str) -> Opts {
    let ssl = match &options.tls {
        TlsMode::Disable => None,
        TlsMode::VerifyFull { ca_file } => {
            let roots = ca_file.iter().cloned().map(Into::into).collect();
            Some(
                SslOpts::default()
                    .with_root_certs(roots)
                    .with_disable_built_in_roots(ca_file.is_some()),
            )
        }
    };
    let socket = options.host.starts_with('/').then(|| options.host.clone());
    OptsBuilder::default()
        .ip_or_hostname(if socket.is_some() {
            "localhost"
        } else {
            &options.host
        })
        .tcp_port(options.port)
        .socket(socket)
        .prefer_socket(false)
        .user(Some(options.user.clone()))
        .pass(
            credentials
                .password
                .as_ref()
                .map(|password| password.expose().to_owned()),
        )
        .db_name((!database.is_empty()).then(|| database.to_owned()))
        .ssl_opts(ssl)
        .client_found_rows(true)
        .into()
}

pub(crate) async fn connect(opts: Opts, timeout: Option<Duration>) -> DbResult<Conn> {
    tokio::time::timeout(timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT), Conn::new(opts))
        .await
        .map_err(|_| DatabaseError::new(ErrorKind::Connection, "The connection timed out"))?
        .map_err(map_error)
}

pub(crate) async fn configure(connection: &mut Conn, access: AccessMode) -> DbResult<()> {
    connection
        .query_drop("SET NAMES utf8mb4")
        .await
        .map_err(map_error)?;
    connection
        .query_drop("SET SESSION time_zone = '+00:00'")
        .await
        .map_err(map_error)?;
    if access == AccessMode::ReadOnly {
        connection
            .query_drop("SET SESSION TRANSACTION READ ONLY")
            .await
            .map_err(map_error)?;
    }
    Ok(())
}
