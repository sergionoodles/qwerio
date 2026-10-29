use std::fs::File;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use qwerio_core::connection::{ConnectionOptions, MysqlOptions, PostgresOptions, TlsMode};

use super::files::PrivateFile;
use super::{Format, Operation, TransferRequest, process, tools};

const SYSTEM_CA_BUNDLES: [&str; 4] = [
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/cert.pem",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
];

pub(super) fn run(request: &TransferRequest, cancelled: &AtomicBool) -> Result<PathBuf, String> {
    let tool = request
        .format
        .tool(request.options.engine(), request.operation)
        .ok_or("This engine does not use native tools")?;
    let executable = request
        .tool
        .as_ref()
        .ok_or("Locate the database client tools first")?;
    tools::check(tool, executable)?;
    let database = request
        .database
        .as_deref()
        .filter(|database| !database.is_empty())
        .ok_or("Choose a database")?;
    let password = request
        .credentials
        .password
        .as_ref()
        .map(|secret| secret.expose())
        .unwrap_or("");
    let (mut command, _credentials_file) = match &request.options {
        ConnectionOptions::Postgres(options) => {
            postgres_command(request, options, database, password)?
        }
        ConnectionOptions::Mysql(options) | ConnectionOptions::Mariadb(options) => {
            mysql_command(request, options, database, password)?
        }
        ConnectionOptions::Sqlite(_) => return Err("SQLite does not use native tools".to_owned()),
    };
    if request.operation == Operation::Restore {
        if !request.file.is_file() {
            return Err("Choose an existing backup file".to_owned());
        }
        let input = File::open(&request.file).map_err(|error| error.to_string())?;
        command.stdin(input).stdout(Stdio::null());
        process::run(&mut command, cancelled, None, false)?;
        return Ok(request.file.clone());
    }
    let directory = request.file.parent().ok_or("Choose a destination folder")?;
    if request.file.exists() {
        return Err("The destination already exists. Choose a new file name.".to_owned());
    }
    let (output, file) = PrivateFile::create(directory)?;
    command.stdin(Stdio::null()).stdout(file);
    process::run(&mut command, cancelled, None, false)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("Cancelled".to_owned());
    }
    output.publish(&request.file)?;
    Ok(request.file.clone())
}

fn postgres_command(
    request: &TransferRequest,
    options: &PostgresOptions,
    database: &str,
    password: &str,
) -> Result<(Command, PrivateFile), String> {
    if password.contains(['\n', '\r', '\0']) {
        return Err("PostgreSQL password files cannot represent this password".to_owned());
    }
    let escaped = password.replace('\\', "\\\\").replace(':', "\\:");
    let credentials = PrivateFile::secret(&format!("*:*:*:*:{escaped}\n"))?;
    let executable = request.tool.as_ref().ok_or("Choose a client tool")?;
    let mut command = Command::new(executable);
    // libpq interprets dbname values containing '=' as connection strings.
    // Use the environment so literal database names cannot override endpoints.
    for (name, _) in
        std::env::vars_os().filter(|(name, _)| name.to_string_lossy().starts_with("PG"))
    {
        command.env_remove(name);
    }
    command
        .env("PGHOST", &options.host)
        .env("PGPORT", options.port.to_string())
        .env("PGUSER", &options.user)
        .env("PGDATABASE", database)
        .env("PGPASSFILE", &credentials.path)
        .env("PGAPPNAME", "Qwerio")
        .arg("--no-password");
    if let Some(timeout) = options.connect_timeout {
        command.env("PGCONNECT_TIMEOUT", timeout.as_secs().max(1).to_string());
    }
    match &options.tls {
        TlsMode::Disable => {
            command.env("PGSSLMODE", "disable");
        }
        TlsMode::VerifyFull { ca_file } => {
            command.env("PGSSLMODE", "verify-full");
            command.env(
                "PGSSLROOTCERT",
                ca_file.clone().unwrap_or_else(|| "system".into()),
            );
        }
    }
    match (request.operation, request.format) {
        (Operation::Backup, Format::Archive) => {
            command.arg("--format=custom");
        }
        (Operation::Backup, Format::Sql) => {
            command.args(["--format=plain", "--clean", "--if-exists"]);
        }
        (Operation::Restore, Format::Archive) => {
            // An empty dbname selects direct restore; libpq uses PGDATABASE.
            command.args(["--dbname=", "--exit-on-error", "--clean", "--if-exists"]);
        }
        (Operation::Restore, Format::Sql) => {
            command.args(["--no-psqlrc", "--set=ON_ERROR_STOP=1"]);
        }
    }
    Ok((command, credentials))
}

fn mysql_command(
    request: &TransferRequest,
    options: &MysqlOptions,
    database: &str,
    password: &str,
) -> Result<(Command, PrivateFile), String> {
    let contents = format!("[client]\npassword={}\n", option_value(password));
    let credentials = PrivateFile::secret(&contents)?;
    let executable = request.tool.as_ref().ok_or("Choose a client tool")?;
    let mut command = Command::new(executable);
    command.arg(format!("--defaults-file={}", credentials.path.display()));
    let is_mariadb = matches!(request.options, ConnectionOptions::Mariadb(_));
    if !is_mariadb {
        // MySQL reads its login-path file even with an explicit defaults file.
        command.arg("--no-login-paths");
    }
    command.env_remove("MYSQL_PWD");
    command.arg(format!("--user={}", options.user));
    if options.host.starts_with('/') {
        command
            .arg("--protocol=SOCKET")
            .arg(format!("--socket={}", options.host));
    } else {
        command.args([
            "--protocol=TCP",
            &format!("--host={}", options.host),
            &format!("--port={}", options.port),
        ]);
    }
    if let Some(timeout) = options.connect_timeout {
        command.arg(format!("--connect-timeout={}", timeout.as_secs().max(1)));
    }
    match (&options.tls, is_mariadb) {
        (TlsMode::Disable, true) => {
            command.arg("--skip-ssl");
        }
        (TlsMode::Disable, false) => {
            command.arg("--ssl-mode=DISABLED");
        }
        (TlsMode::VerifyFull { ca_file }, _) => {
            if is_mariadb {
                command.args(["--ssl", "--ssl-verify-server-cert"]);
            } else {
                command.arg("--ssl-mode=VERIFY_IDENTITY");
            }
            let path = ca_file.clone().or_else(|| SYSTEM_CA_BUNDLES.iter().map(PathBuf::from).find(|path| path.is_file()))
                .ok_or("Native clients require a CA bundle for verified TLS. Set a CA file in Connection Settings.")?;
            command.arg(format!("--ssl-ca={}", path.display()));
        }
    }
    match request.operation {
        Operation::Backup => {
            command.args([
                "--single-transaction",
                "--quick",
                "--routines",
                "--events",
                "--triggers",
                "--hex-blob",
            ]);
            if !is_mariadb {
                command.arg("--set-gtid-purged=OFF");
            }
            command.arg("--").arg(database);
        }
        Operation::Restore => {
            command.args(["--batch", "--binary-mode"]);
            command.arg(format!("--database={database}"));
        }
    }
    Ok((command, credentials))
}

fn option_value(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_file_passwords_cannot_inject_options() {
        assert_eq!(
            option_value("a\"\\b\n[client]\nuser=root"),
            "\"a\\\"\\\\b\\n[client]\\nuser=root\""
        );
    }
}
