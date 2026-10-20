//! Connection failures tested against local protocol stubs, without a database.

use std::time::Duration;

use qwerio_core::connection::{
    ConnectionDescriptor, ConnectionOptions, Credentials, PostgresOptions, TlsMode,
};
use qwerio_core::driver::{DatabaseDriver, OpenSessionRequest};
use qwerio_core::error::ErrorKind;
use qwerio_core::ids::ConnectionId;
use qwerio_core::session::{AccessMode, SessionPurpose};
use qwerio_postgres::PostgresDriver;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 4, 210, 22, 47];

#[tokio::test]
async fn tls_rejection_explains_how_to_change_encryption() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let endpoint = listener.local_addr().expect("local address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut request = [0; SSL_REQUEST.len()];
        stream.read_exact(&mut request).await.expect("SSL request");
        assert_eq!(request, SSL_REQUEST);
        stream.write_all(b"N").await.expect("reject TLS");
    });
    let request = OpenSessionRequest {
        connection: ConnectionDescriptor {
            id: ConnectionId::new(),
            name: "no TLS".to_owned(),
            options: ConnectionOptions::Postgres(PostgresOptions {
                host: endpoint.ip().to_string(),
                port: endpoint.port(),
                user: "test".to_owned(),
                database: "test".to_owned(),
                tls: TlsMode::VerifyFull { ca_file: None },
                connect_timeout: Some(TEST_TIMEOUT),
                application_name: None,
            }),
        },
        credentials: Credentials::password("test-password-never-sent"),
        purpose: SessionPurpose::Browse,
        access: AccessMode::ReadOnly,
        database: None,
    };

    let error = tokio::time::timeout(
        TEST_TIMEOUT,
        PostgresDriver::new(tokio::runtime::Handle::current()).open(request),
    )
    .await
    .expect("connection completes")
    .err()
    .expect("TLS must fail");
    server.await.expect("server task");

    assert_eq!(error.kind, ErrorKind::Tls);
    assert!(error.message.contains("server does not support TLS"));
    assert!(error.message.contains("Encryption"));
    assert!(error.message.contains("No encryption"));
    assert!(!format!("{error:?}").contains("test-password-never-sent"));
}
