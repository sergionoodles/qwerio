//! Connection failures tested against local protocol stubs, without a database.

use std::net::SocketAddr;
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
use tokio::net::{TcpListener, TcpStream};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 4, 210, 22, 47];
const STARTUP_LENGTH_BYTES: usize = std::mem::size_of::<u32>();
const AUTHENTICATED: &[u8] = &[b'R', 0, 0, 0, 8, 0, 0, 0, 0];
const READY_FOR_QUERY: &[u8] = &[b'Z', 0, 0, 0, 5, b'I'];

fn request(endpoint: SocketAddr, tls: TlsMode) -> OpenSessionRequest {
    OpenSessionRequest {
        connection: ConnectionDescriptor {
            id: ConnectionId::new(),
            name: "test".to_owned(),
            options: ConnectionOptions::Postgres(PostgresOptions {
                host: endpoint.ip().to_string(),
                port: endpoint.port(),
                user: "test".to_owned(),
                database: "test".to_owned(),
                tls,
                connect_timeout: Some(TEST_TIMEOUT),
                application_name: None,
            }),
        },
        credentials: Credentials::password("test-password-never-sent"),
        purpose: SessionPurpose::Browse,
        access: AccessMode::ReadOnly,
        database: None,
    }
}

async fn send_error(stream: &mut TcpStream, code: &str, message: &str) {
    let fields = format!("SFATAL\0C{code}\0M{message}\0\0");
    let length = u32::try_from(fields.len() + STARTUP_LENGTH_BYTES).expect("error length");
    stream.write_all(b"E").await.expect("error type");
    stream.write_u32(length).await.expect("error length");
    stream.write_all(fields.as_bytes()).await.expect("error");
}

async fn read_message_body(stream: &mut TcpStream) -> Vec<u8> {
    let length = stream.read_u32().await.expect("message length");
    let mut body = vec![0; usize::try_from(length).expect("message length") - STARTUP_LENGTH_BYTES];
    stream.read_exact(&mut body).await.expect("message body");
    body
}

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
    let error = tokio::time::timeout(
        TEST_TIMEOUT,
        PostgresDriver::new(tokio::runtime::Handle::current())
            .open(request(endpoint, TlsMode::VerifyFull { ca_file: None })),
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

#[tokio::test]
async fn session_defaults_do_not_prevent_startup_through_a_pooler() {
    const AUTHENTICATION_MESSAGE: &str = "Startup accepted; authentication rejected by test server";
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let endpoint = listener.local_addr().expect("local address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let startup = read_message_body(&mut stream).await;
        let has_options = startup[STARTUP_LENGTH_BYTES..]
            .split(|byte| *byte == 0)
            .step_by(2)
            .any(|key| key == b"options");

        if has_options {
            send_error(
                &mut stream,
                "08P01",
                "unsupported startup parameter in options: default_transaction_read_only",
            )
            .await;
            return;
        }

        send_error(&mut stream, "28P01", AUTHENTICATION_MESSAGE).await;
    });

    let error = tokio::time::timeout(
        TEST_TIMEOUT,
        PostgresDriver::new(tokio::runtime::Handle::current())
            .open(request(endpoint, TlsMode::Disable)),
    )
    .await
    .expect("connection completes")
    .err()
    .expect("test server rejects authentication after startup");
    server.await.expect("server task");

    assert_eq!(error.kind, ErrorKind::Authentication, "{error}");
    assert_eq!(error.message, AUTHENTICATION_MESSAGE);
}

#[tokio::test]
async fn session_configuration_errors_prevent_opening_a_session() {
    const SETTING_ERROR_MESSAGE: &str = "permission denied to set parameter statement_timeout";
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let endpoint = listener.local_addr().expect("local address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let _startup = read_message_body(&mut stream).await;
        stream.write_all(AUTHENTICATED).await.expect("authenticate");
        stream.write_all(READY_FOR_QUERY).await.expect("ready");

        let _message_type = stream.read_u8().await.expect("query message");
        let _query = read_message_body(&mut stream).await;
        send_error(&mut stream, "42501", SETTING_ERROR_MESSAGE).await;
        stream.write_all(READY_FOR_QUERY).await.expect("ready");
    });

    let error = tokio::time::timeout(
        TEST_TIMEOUT,
        PostgresDriver::new(tokio::runtime::Handle::current())
            .open(request(endpoint, TlsMode::Disable)),
    )
    .await
    .expect("connection completes")
    .err()
    .expect("session configuration must succeed before opening");
    server.await.expect("server task");

    assert_eq!(error.kind, ErrorKind::Permission);
    assert_eq!(error.message, SETTING_ERROR_MESSAGE);
}
