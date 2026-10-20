//! Establishing PostgreSQL connections with certificate-validating TLS.

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use qwerio_core::connection::{Credentials, PostgresOptions, TlsMode};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::session::{AccessMode, SessionPurpose};
use rustls::ClientConfig;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use tokio_postgres::config::SslMode;
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::{CancelToken, Client, Config, Connection, NoTls, Socket};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::error::map_error;

const APPLICATION_NAME: &str = "Qwerio";

/// Generated browsing queries stop after this long on the server.
const BROWSE_STATEMENT_TIMEOUT_MS: u32 = 60_000;

pub(crate) struct Connected {
    pub client: Client,
    pub cancel_token: CancelToken,
    pub tls: Option<MakeRustlsConnect>,
    pub server_version: Option<String>,
}

pub(crate) struct ConnectRequest<'a> {
    pub options: &'a PostgresOptions,
    pub credentials: &'a Credentials,
    pub database: &'a str,
    pub purpose: SessionPurpose,
    pub access: AccessMode,
}

pub(crate) async fn connect(request: ConnectRequest<'_>) -> DbResult<Connected> {
    let config = build_config(&request);

    let (client, tls) = match &request.options.tls {
        TlsMode::Disable => {
            let (client, connection) = connect_transport(&config, request.options, NoTls).await?;
            tokio::spawn(connection);
            (client, None)
        }
        TlsMode::VerifyFull { ca_file } => {
            let tls = tls_connector(ca_file.as_deref())?;
            let (client, connection) =
                connect_transport(&config, request.options, tls.clone()).await?;
            tokio::spawn(connection);
            (client, Some(tls))
        }
    };

    configure_session(&client, &request).await?;

    let server_version = client
        .query_one("SHOW server_version", &[])
        .await
        .ok()
        .and_then(|row| row.try_get::<_, String>(0).ok());

    Ok(Connected {
        cancel_token: client.cancel_token(),
        client,
        tls,
        server_version,
    })
}

async fn connect_transport<T>(
    config: &Config,
    options: &PostgresOptions,
    tls: T,
) -> DbResult<(Client, Connection<Socket, T::Stream>)>
where
    T: MakeTlsConnect<Socket> + Clone,
{
    if options.is_unix_socket() {
        return config
            .connect(tls)
            .await
            .map_err(|error| map_error(error, None));
    }

    let addresses = tokio::net::lookup_host((options.host.as_str(), options.port))
        .await
        .map_err(|error| {
            DatabaseError::new(
                ErrorKind::Connection,
                format!("Could not resolve the database host: {error}"),
            )
        })?;

    connect_addresses(config, addresses.map(|address| address.ip()), tls).await
}

async fn connect_addresses<T>(
    config: &Config,
    addresses: impl IntoIterator<Item = IpAddr>,
    tls: T,
) -> DbResult<(Client, Connection<Socket, T::Stream>)>
where
    T: MakeTlsConnect<Socket> + Clone,
{
    let mut failure: Option<DatabaseError> = None;

    for address in addresses {
        let mut address_config = config.clone();
        // Keep the original hostname for certificate validation and SNI.
        address_config.hostaddr(address);

        match address_config.connect(tls.clone()).await {
            Ok(connected) => return Ok(connected),
            Err(error) => {
                let mapped = map_error(error, None);
                // A later unreachable address must not hide a TLS or server
                // error from an address we did reach.
                if failure.as_ref().is_none_or(|previous| {
                    previous.kind == ErrorKind::Connection && previous.code.is_none()
                }) {
                    failure = Some(mapped);
                }
            }
        }
    }

    Err(failure.unwrap_or_else(|| {
        DatabaseError::new(
            ErrorKind::Connection,
            "The database host did not resolve to any addresses",
        )
    }))
}

fn build_config(request: &ConnectRequest<'_>) -> Config {
    let options = request.options;
    let mut config = Config::new();
    config
        .host(&options.host)
        .port(options.port)
        .user(&options.user)
        .dbname(request.database)
        .application_name(
            options
                .application_name
                .as_deref()
                .unwrap_or(APPLICATION_NAME),
        );

    if let Some(password) = &request.credentials.password {
        config.password(password.expose());
    }

    if let Some(timeout) = options.connect_timeout {
        config.connect_timeout(timeout);
    }

    config.ssl_mode(match options.tls {
        TlsMode::Disable => SslMode::Disable,
        TlsMode::VerifyFull { .. } => SslMode::Require,
    });

    config
}

async fn configure_session(client: &Client, request: &ConnectRequest<'_>) -> DbResult<()> {
    let mut server_settings = Vec::new();

    // Some poolers reject these settings in the startup options. Apply them
    // after connecting, before exposing the session to callers.
    // A session default, not a security boundary: arbitrary SQL can change
    // it. Generated browsing queries never do.
    if request.access == AccessMode::ReadOnly {
        server_settings.push("SET default_transaction_read_only = on".to_owned());
    }

    if request.purpose == SessionPurpose::Browse {
        server_settings.push(format!(
            "SET statement_timeout = {BROWSE_STATEMENT_TIMEOUT_MS}"
        ));
    }

    if server_settings.is_empty() {
        return Ok(());
    }

    client
        .batch_execute(&server_settings.join("; "))
        .await
        .map_err(|error| map_error(error, None))
}

/// A rustls connector that validates the certificate chain against the
/// platform trust store (plus an optional CA bundle) and the host name.
fn tls_connector(ca_file: Option<&Path>) -> DbResult<MakeRustlsConnect> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let extra_roots = match ca_file {
        Some(path) => read_certificates(path)?,
        None => Vec::new(),
    };

    let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots(
        extra_roots,
        Arc::clone(&provider),
    )
    .map_err(tls_error)?;

    let config = ClientConfig::builder_with_provider(provider as Arc<CryptoProvider>)
        .with_safe_default_protocol_versions()
        .map_err(tls_error)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();

    Ok(MakeRustlsConnect::new(config))
}

fn read_certificates(path: &Path) -> DbResult<Vec<CertificateDer<'static>>> {
    let certificates = CertificateDer::pem_file_iter(path)
        .and_then(|iterator| iterator.collect::<Result<Vec<_>, _>>())
        .map_err(|error| {
            DatabaseError::new(
                ErrorKind::Tls,
                format!(
                    "Could not read CA certificates from {}: {error}",
                    path.display()
                ),
            )
        })?;

    if certificates.is_empty() {
        return Err(DatabaseError::new(
            ErrorKind::Tls,
            format!("No certificates found in {}", path.display()),
        ));
    }

    Ok(certificates)
}

fn tls_error(error: rustls::Error) -> DatabaseError {
    DatabaseError::new(ErrorKind::Tls, format!("TLS setup failed: {error}"))
}

#[cfg(test)]
mod tests;
