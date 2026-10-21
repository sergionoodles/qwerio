use super::*;

fn postgres(input: &str) -> (PostgresOptions, Option<String>) {
    let parsed = parse_connection_string(input).expect("a valid connection string");
    let password = parsed.password.map(|secret| secret.expose().to_owned());
    (parsed.options, password)
}

#[test]
fn reads_every_part_of_a_postgres_url() {
    let (options, password) =
        postgres("postgres://app:s3cret@db.example.com:6543/shop?sslmode=require");

    assert_eq!(options.host, "db.example.com");
    assert_eq!(options.port, 6543);
    assert_eq!(options.user, "app");
    assert_eq!(options.database, "shop");
    assert_eq!(options.tls, TlsMode::VerifyFull { ca_file: None });
    assert_eq!(password.as_deref(), Some("s3cret"));
}

#[test]
fn applies_libpq_defaults_for_missing_parts() {
    let (options, password) = postgres("postgresql://app@localhost");

    assert_eq!(options.port, DEFAULT_POSTGRES_PORT);
    assert_eq!(options.database, "app");
    assert_eq!(password, None);
}

#[test]
fn decodes_escapes_and_unescaped_at_signs_in_passwords() {
    let (options, password) = postgres("postgres://app%40corp:p@ss%2Fword@host/my%20db");

    assert_eq!(options.user, "app@corp");
    assert_eq!(password.as_deref(), Some("p@ss/word"));
    assert_eq!(options.host, "host");
    assert_eq!(options.database, "my db");
}

#[test]
fn reads_ipv6_hosts_and_the_first_of_several_hosts() {
    let (ipv6, _) = postgres("postgres://app@[::1]:5433/shop");
    let (several, _) = postgres("postgres://app@primary:5433,replica:5434/shop");

    assert_eq!((ipv6.host.as_str(), ipv6.port), ("::1", 5433));
    assert_eq!((several.host.as_str(), several.port), ("primary", 5433));
}

#[test]
fn query_parameters_override_the_url_and_set_a_socket_host() {
    let (options, password) = postgres(
        "postgres:///shop?host=%2Fvar%2Frun%2Fpostgresql&user=app&password=pw&schema=public",
    );

    assert_eq!(options.host, "/var/run/postgresql");
    assert_eq!(options.user, "app");
    assert_eq!(options.database, "shop");
    assert_eq!(password.as_deref(), Some("pw"));
}

#[test]
fn chooses_encryption_from_sslmode_and_the_host() {
    let tls = |input: &str| postgres(input).0.tls;
    let verify = TlsMode::VerifyFull { ca_file: None };

    assert_eq!(tls("postgres://localhost/shop"), TlsMode::Disable);
    assert_eq!(tls("postgres://db.example.com/shop"), verify);
    assert_eq!(
        tls("postgres://db.example.com/shop?sslmode=disable"),
        TlsMode::Disable
    );
    assert_eq!(tls("postgres://localhost/shop?sslmode=verify-ca"), verify);
    assert_eq!(
        tls("postgres://db.example.com/shop?sslmode=verify-full&sslrootcert=/etc/ca.pem"),
        TlsMode::VerifyFull {
            ca_file: Some(PathBuf::from("/etc/ca.pem"))
        }
    );
}

#[test]
fn accepts_env_lines_and_jdbc_prefixes() {
    let (from_env, _) = postgres("export DATABASE_URL=\"postgres://app@db/shop\"");
    let (from_jdbc, _) = postgres("jdbc:postgresql://db:5433/shop");

    assert_eq!(
        (from_env.host.as_str(), from_env.database.as_str()),
        ("db", "shop")
    );
    assert_eq!((from_jdbc.host.as_str(), from_jdbc.port), ("db", 5433));
}

#[test]
fn rejects_strings_it_cannot_read() {
    let error = |input: &str| parse_connection_string(input).expect_err("an invalid string");

    assert_eq!(error("localhost"), ConnectionStringError::MissingScheme);
    assert_eq!(
        error("mysql://db/shop"),
        ConnectionStringError::UnsupportedScheme("mysql".to_owned())
    );
    assert_eq!(
        error("postgres://db:port/shop"),
        ConnectionStringError::InvalidPort("port".to_owned())
    );
    assert_eq!(
        error("postgres://app:%zz@db/shop"),
        ConnectionStringError::InvalidEscape
    );
    assert_eq!(
        error("postgres://db/shop?sslmode=sometimes"),
        ConnectionStringError::UnsupportedSslMode("sometimes".to_owned())
    );
    assert_eq!(
        error("sqlite:///data/app.db"),
        ConnectionStringError::UnsupportedScheme("sqlite".to_owned())
    );
}

#[test]
fn reads_mysql_and_mariadb_urls_without_postgres_defaults() {
    for scheme in ["mysql", "mariadb"] {
        let parsed = parse_mysql_connection_string(&format!(
            "{scheme}://app:p%40ss@[::1]/shop?ssl-mode=VERIFY_IDENTITY"
        ))
        .expect("URL");
        assert_eq!(parsed.options.host, "::1");
        assert_eq!(parsed.options.port, 3306);
        assert_eq!(parsed.options.database, "shop");
        assert_eq!(parsed.password.as_ref().map(Secret::expose), Some("p@ss"));
        assert_eq!(parsed.options.tls, TlsMode::VerifyFull { ca_file: None });
    }
    let parsed = parse_mysql_connection_string("mysql://root@localhost").expect("no database");
    assert_eq!(parsed.options.database, "");
    assert_eq!(parsed.options.tls, TlsMode::Disable);
}

#[test]
fn mysql_urls_validate_ports_escapes_and_tls() {
    for url in [
        "mysql://root@localhost:0/db",
        "mysql://root@localhost:bad/db",
        "mysql://root@localhost/%zz",
        "mysql://root@localhost/db?verify_identity=false",
        "postgres://root@localhost/db",
    ] {
        assert!(parse_mysql_connection_string(url).is_err(), "{url}");
    }
    let parsed = parse_mysql_connection_string(
        "jdbc:mariadb://app@host/db?ssl-mode=REQUIRED&ssl-ca=%2Ftmp%2Fca.pem",
    )
    .expect("TLS URL");
    assert_eq!(
        parsed.options.tls,
        TlsMode::VerifyFull {
            ca_file: Some("/tmp/ca.pem".into())
        }
    );
}
