use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const REFUSED_ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 4, 210, 22, 47];
const AUTHENTICATED_AND_READY: &[u8] = &[b'R', 0, 0, 0, 8, 0, 0, 0, 0, b'Z', 0, 0, 0, 5, b'I'];
const STARTUP_LENGTH_BYTES: usize = std::mem::size_of::<u32>();
const STARTUP_ERROR_MESSAGE: &str =
    "unsupported startup parameter in options: default_transaction_read_only";

fn config(port: u16, ssl_mode: SslMode) -> Config {
    let mut config = Config::new();
    config
        .host("localhost")
        .port(port)
        .user("test")
        .dbname("test")
        .ssl_mode(ssl_mode)
        .connect_timeout(TEST_TIMEOUT);
    config
}

async fn reject_tls() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let endpoint = listener.local_addr().expect("local address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut request = [0; SSL_REQUEST.len()];
        stream.read_exact(&mut request).await.expect("SSL request");
        assert_eq!(request, SSL_REQUEST);
        stream.write_all(b"N").await.expect("reject TLS");
    });
    (endpoint, server)
}

#[tokio::test]
async fn tls_rejection_survives_other_unreachable_addresses_in_either_order() {
    for refused_first in [false, true] {
        let (endpoint, server) = reject_tls().await;
        let addresses = if refused_first {
            [REFUSED_ADDRESS, endpoint.ip()]
        } else {
            [endpoint.ip(), REFUSED_ADDRESS]
        };
        let error = connect_addresses(
            &config(endpoint.port(), SslMode::Require),
            addresses,
            tls_connector(None).expect("TLS connector"),
        )
        .await
        .err()
        .expect("TLS must fail");
        server.await.expect("server task");

        assert_eq!(error.kind, ErrorKind::Tls);
        assert!(error.message.contains("server does not support TLS"));
        assert!(error.message.contains("No encryption"));
    }
}

#[tokio::test]
async fn server_connection_errors_survive_other_unreachable_addresses_in_either_order() {
    for refused_first in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let endpoint = listener.local_addr().expect("local address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let length = stream.read_u32().await.expect("startup length");
            let mut startup =
                vec![0; usize::try_from(length).expect("length") - STARTUP_LENGTH_BYTES];
            stream.read_exact(&mut startup).await.expect("startup");
            let fields = format!("SFATAL\0C08P01\0M{STARTUP_ERROR_MESSAGE}\0\0");
            let length =
                u32::try_from(fields.len() + STARTUP_LENGTH_BYTES).expect("error length");
            stream.write_all(b"E").await.expect("error type");
            stream.write_u32(length).await.expect("error length");
            stream.write_all(fields.as_bytes()).await.expect("error");
        });
        let addresses = if refused_first {
            [REFUSED_ADDRESS, endpoint.ip()]
        } else {
            [endpoint.ip(), REFUSED_ADDRESS]
        };
        let error = connect_addresses(
            &config(endpoint.port(), SslMode::Disable),
            addresses,
            NoTls,
        )
        .await
        .err()
        .expect("server rejects startup");
        server.await.expect("server task");

        assert_eq!(error.code.as_deref(), Some("08P01"), "{error}");
        assert_eq!(error.message, STARTUP_ERROR_MESSAGE);
    }
}

#[tokio::test]
async fn a_later_reachable_address_can_still_connect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let endpoint = listener.local_addr().expect("local address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let length = stream.read_u32().await.expect("startup length");
        let mut startup = vec![0; usize::try_from(length).expect("length") - 4];
        stream.read_exact(&mut startup).await.expect("startup");
        stream
            .write_all(AUTHENTICATED_AND_READY)
            .await
            .expect("authenticate");
    });

    let result = connect_addresses(
        &config(endpoint.port(), SslMode::Disable),
        [REFUSED_ADDRESS, endpoint.ip()],
        NoTls,
    )
    .await;
    assert!(
        result.is_ok(),
        "fallback should connect: {:?}",
        result.err()
    );
    server.await.expect("server task");
}

#[tokio::test]
async fn unreachable_addresses_remain_connection_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let endpoint = listener.local_addr().expect("local address");
    let error = connect_addresses(
        &config(endpoint.port(), SslMode::Disable),
        [REFUSED_ADDRESS],
        NoTls,
    )
    .await
    .err()
    .expect("connection refused");

    assert_eq!(error.kind, ErrorKind::Connection);
    assert!(!error.message.contains("No encryption"));
}

#[tokio::test]
async fn empty_address_results_explain_the_resolution_failure() {
    let error = connect_addresses(&config(5432, SslMode::Disable), [], NoTls)
        .await
        .err()
        .expect("no addresses");

    assert_eq!(error.kind, ErrorKind::Connection);
    assert!(error.message.contains("did not resolve to any addresses"));
}
