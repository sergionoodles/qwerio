//! MySQL and MariaDB adapters, each session retaining one physical connection.
//!
//! Atomic editing requires InnoDB storage for the target and any tables written
//! by its triggers or stored routines. Target storage is validated before editing;
//! arbitrary definer routines' external/nontransactional effects cannot be undone.

mod catalog;
mod connect;
mod decode;
mod dialect;
mod edits;
mod error;
mod execute;
mod session;
mod sql_text;

use crate::session::MysqlSession;
use async_trait::async_trait;
use mysql_async::prelude::Queryable;
use qwerio_core::capabilities::{CancellationSupport, DatabaseCapabilities};
use qwerio_core::connection::{ConnectionOptions, EngineKind, TlsMode};
use qwerio_core::driver::{DatabaseDriver, DatabaseSession, OpenSessionRequest, ValidationIssue};
use qwerio_core::error::{DatabaseError, DbResult};
use qwerio_core::filter::FilterOperator;
use qwerio_core::ids::SessionId;
use qwerio_core::session::{AccessMode, ReadOnlyEnforcement, SessionInfo};
use std::sync::Arc;
use tokio::runtime::Handle;

const COLUMN_TYPES: [&str; 10] = [
    "text",
    "int",
    "bigint",
    "decimal(30,10)",
    "boolean",
    "date",
    "datetime(6)",
    "varchar(255)",
    "json",
    "blob",
];

pub struct MysqlDriver {
    runtime: Handle,
    engine: EngineKind,
    capabilities: DatabaseCapabilities,
}

impl MysqlDriver {
    pub fn new(runtime: Handle) -> Self {
        Self::for_engine(runtime, EngineKind::Mysql)
    }

    pub fn mariadb(runtime: Handle) -> Self {
        Self::for_engine(runtime, EngineKind::Mariadb)
    }

    fn for_engine(runtime: Handle, engine: EngineKind) -> Self {
        Self {
            runtime,
            engine,
            capabilities: DatabaseCapabilities {
                has_databases: true,
                has_namespaces: false,
                access_modes: vec![AccessMode::ReadOnly, AccessMode::ReadWrite],
                read_only_enforcement: ReadOnlyEnforcement::TransactionDefault,
                transactional_ddl: false,
                cancellation: CancellationSupport::ServerCancelRequest,
                filter_operators: FilterOperator::ALL.to_vec(),
                unicode_case_folding: true,
                errors_abort_transactions: false,
                column_types: COLUMN_TYPES.iter().map(|name| (*name).to_owned()).collect(),
                key_column_type: "bigint AUTO_INCREMENT".to_owned(),
            },
        }
    }
}

#[async_trait]
impl DatabaseDriver for MysqlDriver {
    fn engine(&self) -> EngineKind {
        self.engine
    }

    fn capabilities(&self) -> &DatabaseCapabilities {
        &self.capabilities
    }

    fn validate(&self, options: &ConnectionOptions) -> Vec<ValidationIssue> {
        if options.engine() != self.engine {
            return vec![ValidationIssue {
                field: "engine",
                message: "The connection options belong to a different engine".to_owned(),
            }];
        }
        let (ConnectionOptions::Mysql(options) | ConnectionOptions::Mariadb(options)) = options
        else {
            return vec![ValidationIssue {
                field: "engine",
                message: "Expected MySQL or MariaDB options".to_owned(),
            }];
        };
        let mut issues = Vec::new();
        for (field, value) in [("host", &options.host), ("user", &options.user)] {
            if value.trim().is_empty() {
                issues.push(ValidationIssue {
                    field,
                    message: format!("Enter a {field}"),
                });
            }
        }
        if options.port == 0 {
            issues.push(ValidationIssue {
                field: "port",
                message: "Enter a port between 1 and 65535".to_owned(),
            });
        }
        if options.host.starts_with('/') && !matches!(options.tls, TlsMode::Disable) {
            issues.push(ValidationIssue {
                field: "tls",
                message: "Unix socket connections do not use TLS".to_owned(),
            });
        }
        issues
    }

    async fn open(&self, request: OpenSessionRequest) -> DbResult<Arc<dyn DatabaseSession>> {
        if let Some(issue) = self.validate(&request.connection.options).first() {
            return Err(DatabaseError::invalid_request(issue.message.clone()));
        }
        let (ConnectionOptions::Mysql(options) | ConnectionOptions::Mariadb(options)) =
            &request.connection.options
        else {
            return Err(DatabaseError::invalid_request(
                "Expected MySQL or MariaDB options",
            ));
        };
        let database = request
            .database
            .clone()
            .unwrap_or_else(|| options.database.clone());
        let opts = connect::options(options, &request.credentials, &database);
        let control_opts = connect::options(options, &request.credentials, "");
        let timeout = options.connect_timeout;
        let access = request.access;
        let (connection, version) = self
            .runtime
            .spawn(async move {
                let mut connection = connect::connect(opts, timeout).await?;
                connect::configure(&mut connection, access).await?;
                let version: Option<String> = connection
                    .query_first("SELECT VERSION()")
                    .await
                    .map_err(error::map_error)?;
                Ok::<_, DatabaseError>((connection, version))
            })
            .await
            .map_err(|error| {
                DatabaseError::internal(format!("The connect task failed: {error}"))
            })??;
        let info = SessionInfo {
            id: SessionId::new(),
            connection: request.connection.id,
            engine: self.engine,
            database: (!database.is_empty()).then_some(database),
            purpose: request.purpose,
            requested_access: access,
            enforcement: if access == AccessMode::ReadOnly {
                ReadOnlyEnforcement::TransactionDefault
            } else {
                ReadOnlyEnforcement::NotEnforced
            },
            server_version: version,
            foreign_keys_enforced: None,
        };
        Ok(Arc::new(MysqlSession::new(
            info,
            self.runtime.clone(),
            connection,
            control_opts,
            timeout,
        )))
    }
}
