use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::process;

const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeTool {
    PgDump,
    PgRestore,
    Psql,
    MysqlDump,
    Mysql,
    MariadbDump,
    Mariadb,
}

impl NativeTool {
    pub fn name(self) -> &'static str {
        match self {
            Self::PgDump => "pg_dump",
            Self::PgRestore => "pg_restore",
            Self::Psql => "psql",
            Self::MysqlDump => "mysqldump",
            Self::Mysql => "mysql",
            Self::MariadbDump => "mariadb-dump",
            Self::Mariadb => "mariadb",
        }
    }

    fn matches(self, version: &str) -> bool {
        let name_matches = version.contains(self.name());
        match self {
            Self::PgDump | Self::PgRestore | Self::Psql => {
                name_matches && version.contains("PostgreSQL")
            }
            Self::Mariadb | Self::MariadbDump => name_matches && version.contains("MariaDB"),
            Self::Mysql | Self::MysqlDump => name_matches && !version.contains("MariaDB"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DetectedTool {
    pub path: PathBuf,
    pub version: String,
}

pub fn detect(tool: NativeTool, directory: Option<&Path>) -> Result<DetectedTool, String> {
    if directory.is_some_and(|directory| !directory.is_absolute()) {
        return Err("Use an absolute path for the native tools folder".to_owned());
    }
    let directories = directory.map_or_else(search_directories, |path| vec![path.to_owned()]);
    let mut last_error = None;
    for directory in directories {
        let path = directory.join(tool.name());
        if !path.is_file() {
            continue;
        }
        match check(tool, &path) {
            Ok(version) => return Ok(DetectedTool { path, version }),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        format!(
            "{} was not found. Install the database client tools or locate their bin folder.",
            tool.name()
        )
    }))
}

pub fn check(tool: NativeTool, path: &Path) -> Result<String, String> {
    let version = process::run(
        Command::new(path).arg("--version").stdin(Stdio::null()),
        &AtomicBool::new(false),
        Some(CHECK_TIMEOUT),
        true,
    )?;
    if !tool.matches(&version) {
        return Err(format!("The selected executable is not {}", tool.name()));
    }
    Ok(version)
}

fn search_directories() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .filter(|path| path.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    paths.extend(
        [
            "/usr/bin",
            "/usr/local/bin",
            "/opt/homebrew/bin",
            "/opt/homebrew/opt/libpq/bin",
            "/usr/local/opt/libpq/bin",
            "/opt/homebrew/opt/mysql-client/bin",
            "/usr/local/opt/mysql-client/bin",
            "/opt/homebrew/opt/mariadb/bin",
            "/usr/local/opt/mariadb/bin",
        ]
        .map(PathBuf::from),
    );
    for major in (14..=19).rev() {
        paths.push(format!("/usr/lib/postgresql/{major}/bin").into());
        paths.push(format!("/opt/homebrew/opt/postgresql@{major}/bin").into());
        paths.push(format!("/usr/local/opt/postgresql@{major}/bin").into());
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_client_families() {
        assert!(NativeTool::PgDump.matches("pg_dump (PostgreSQL) 18.0"));
        assert!(!NativeTool::PgDump.matches("psql (PostgreSQL) 18.0"));
        assert!(!NativeTool::Mysql.matches("mysql Ver 15.1 Distrib 10.11-MariaDB"));
        assert!(NativeTool::Mariadb.matches("mariadb Ver 15.1 Distrib 10.11-MariaDB"));
    }
}
