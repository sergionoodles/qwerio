//! Typed administration requests. Passwords are never included in previews.

mod discovery;
mod execution;
mod mariadb;
mod render;
mod summary;
#[cfg(test)]
mod tests;

pub use discovery::{grants_query, memberships_query, principals_query};
pub(crate) use execution::execute_change;
pub use execution::{PermissionSnapshot, apply, inspect};
pub use summary::{DirectGrant, RoleMembership};

use crate::connection::{EngineKind, Secret};
use crate::error::{DatabaseError, DbResult, ErrorKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub name: String,
    /// MySQL accounts are identified by both user and host. MariaDB roles
    /// and PostgreSQL roles have no host.
    pub host: Option<String>,
}

impl Principal {
    pub fn label(&self) -> String {
        match &self.host {
            Some(host) => format!("{}@{host}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrincipalInfo {
    pub principal: Principal,
    pub description: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    Server,
    Database,
    Schema,
    Table,
    Column,
    Sequence,
    Function,
    Procedure,
}

impl ScopeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Server => "Server",
            Self::Database => "Database",
            Self::Schema => "Schema",
            Self::Table => "Table / view",
            Self::Column => "Column",
            Self::Sequence => "Sequence",
            Self::Function => "Function",
            Self::Procedure => "Procedure",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionScope {
    pub kind: ScopeKind,
    pub database: String,
    pub namespace: String,
    pub object: String,
    pub column: String,
}

impl PermissionScope {
    pub fn server() -> Self {
        Self {
            kind: ScopeKind::Server,
            database: String::new(),
            namespace: String::new(),
            object: String::new(),
            column: String::new(),
        }
    }

    pub fn session_database(&self) -> Option<String> {
        (self.kind != ScopeKind::Server && !self.database.is_empty()).then(|| self.database.clone())
    }

    pub fn available_kinds(engine: EngineKind) -> &'static [ScopeKind] {
        use ScopeKind::*;
        match engine {
            EngineKind::Postgres => &[
                Database, Schema, Table, Column, Sequence, Function, Procedure,
            ],
            EngineKind::Mysql | EngineKind::Mariadb => {
                &[Server, Database, Table, Column, Function, Procedure]
            }
            EngineKind::Sqlite => &[],
        }
    }

    pub fn privileges(&self, engine: EngineKind) -> &'static [&'static str] {
        use ScopeKind::*;
        match (engine, self.kind) {
            (EngineKind::Postgres, Database) => &["CONNECT", "CREATE", "TEMPORARY"],
            (EngineKind::Postgres, Schema) => &["USAGE", "CREATE"],
            (EngineKind::Postgres, Table) => &[
                "SELECT",
                "INSERT",
                "UPDATE",
                "DELETE",
                "TRUNCATE",
                "REFERENCES",
                "TRIGGER",
            ],
            (EngineKind::Postgres, Column) => &["SELECT", "INSERT", "UPDATE", "REFERENCES"],
            (EngineKind::Postgres, Sequence) => &["USAGE", "SELECT", "UPDATE"],
            (EngineKind::Postgres, Function | Procedure) => &["EXECUTE"],
            (EngineKind::Mysql | EngineKind::Mariadb, Server) => &[
                "SELECT",
                "INSERT",
                "UPDATE",
                "DELETE",
                "CREATE",
                "ALTER",
                "DROP",
                "INDEX",
                "EXECUTE",
                "CREATE USER",
                "SHOW DATABASES",
            ],
            (EngineKind::Mysql | EngineKind::Mariadb, Database) => &[
                "SELECT",
                "INSERT",
                "UPDATE",
                "DELETE",
                "CREATE",
                "ALTER",
                "DROP",
                "INDEX",
                "REFERENCES",
                "TRIGGER",
                "EXECUTE",
                "CREATE VIEW",
                "SHOW VIEW",
                "CREATE ROUTINE",
                "ALTER ROUTINE",
                "CREATE TEMPORARY TABLES",
                "LOCK TABLES",
                "EVENT",
            ],
            (EngineKind::Mysql | EngineKind::Mariadb, Table) => &[
                "SELECT",
                "INSERT",
                "UPDATE",
                "DELETE",
                "CREATE",
                "ALTER",
                "DROP",
                "INDEX",
                "REFERENCES",
                "TRIGGER",
                "CREATE VIEW",
                "SHOW VIEW",
            ],
            (EngineKind::Mysql | EngineKind::Mariadb, Column) => {
                &["SELECT", "INSERT", "UPDATE", "REFERENCES"]
            }
            (EngineKind::Mysql | EngineKind::Mariadb, Function | Procedure) => {
                &["EXECUTE", "ALTER ROUTINE"]
            }
            _ => &[],
        }
    }
}

#[derive(Clone, Debug)]
pub enum PermissionChange {
    CreateUser {
        principal: Principal,
        password: Secret,
    },
    CreateRole {
        role: Principal,
    },
    Membership {
        role: Principal,
        member: Principal,
        revoke: bool,
    },
    DefaultRole {
        role: Principal,
        member: Principal,
    },
    Privilege {
        principal: Principal,
        scope: PermissionScope,
        privilege: String,
        revoke: bool,
        grant_option: bool,
    },
}

impl PermissionChange {
    pub fn session_database(&self) -> Option<String> {
        match self {
            Self::Privilege { scope, .. } => scope.session_database(),
            _ => None,
        }
    }

    pub fn requires_roles(&self) -> bool {
        matches!(
            self,
            Self::CreateRole { .. } | Self::Membership { .. } | Self::DefaultRole { .. }
        )
    }
}

pub fn ensure_supported(engine: EngineKind) -> DbResult<()> {
    if engine == EngineKind::Sqlite {
        return Err(DatabaseError::new(
            ErrorKind::Unsupported,
            "SQLite has no database users, roles, or object permissions",
        ));
    }
    Ok(())
}

/// Database-level grants interpret SQL wildcards even inside backticks.
pub fn database_grant_pattern(name: &str) -> String {
    name.replace('\\', "\\\\")
        .replace('_', "\\_")
        .replace('%', "\\%")
}

/// Version strings can include vendor suffixes and MariaDB's 5.5.5 prefix.
pub fn supports_roles(engine: EngineKind, version: &str) -> bool {
    let version = version.strip_prefix("5.5.5-").unwrap_or(version);
    let numbers: Vec<u32> = version
        .split(['.', '-'])
        .take(3)
        .map(|part| part.parse().unwrap_or(0))
        .collect();
    let major = numbers.first().copied().unwrap_or(0);
    match engine {
        EngineKind::Postgres => true,
        EngineKind::Mysql => major >= 8,
        EngineKind::Mariadb => {
            (
                major,
                numbers.get(1).copied().unwrap_or(0),
                numbers.get(2).copied().unwrap_or(0),
            ) >= (10, 0, 5)
        }
        EngineKind::Sqlite => false,
    }
}
