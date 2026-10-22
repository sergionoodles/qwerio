use crate::Value;
use crate::driver::{DatabaseSession, QueryRequest};
use crate::result::{RequestEventKind, Row};
use crate::session::{AccessMode, TransactionState};

use super::*;

#[derive(Clone, Debug)]
pub struct PermissionSnapshot {
    pub database: Option<String>,
    pub supports_roles: bool,
    pub literal_database_names: bool,
    pub principals: Vec<PrincipalInfo>,
    pub memberships: Vec<Row>,
    pub grants: Vec<Row>,
    pub warnings: Vec<String>,
}

/// This session must be dedicated to administration: its SQL literal mode
/// is set explicitly, without modifying a user's query console.
pub async fn inspect(
    session: &dyn DatabaseSession,
    scope: Option<&PermissionScope>,
) -> DbResult<PermissionSnapshot> {
    let engine = session.info().engine;
    ensure_supported(engine)?;
    prepare(session).await?;
    let literal_database_names = if engine == EngineKind::Mysql {
        collect(
            session,
            "SHOW GLOBAL VARIABLES LIKE 'partial_revokes'".to_owned(),
        )
        .await?
        .first()
        .and_then(|row| row.get(1))
        .and_then(Value::canonical_text)
        .is_some_and(|value| value.eq_ignore_ascii_case("ON"))
    } else {
        false
    };
    let supports_roles = supports_roles(
        engine,
        session.info().server_version.as_deref().unwrap_or(""),
    );
    let mut warnings = Vec::new();
    let rows = catalog(
        session,
        principals_query(engine, supports_roles)?,
        "Users and roles",
        &mut warnings,
    )
    .await?;
    let principals = rows
        .into_iter()
        .map(|row| {
            Ok(PrincipalInfo {
                principal: Principal {
                    name: text(&row, 0)?,
                    host: match row.get(1) {
                        Some(Value::Null) => None,
                        Some(_) => Some(text(&row, 1)?),
                        None => return Err(DatabaseError::internal("Missing account host")),
                    },
                },
                description: text(&row, 2)?,
            })
        })
        .collect::<DbResult<Vec<_>>>()?;
    let memberships = if supports_roles {
        catalog(
            session,
            memberships_query(engine)?,
            "Role memberships",
            &mut warnings,
        )
        .await?
    } else {
        Vec::new()
    };
    let grants = match scope {
        Some(scope) => {
            ensure_database(session, scope)?;
            ensure_routine(session, scope).await?;
            catalog(
                session,
                grants_query(engine, scope)?,
                "Direct grants",
                &mut warnings,
            )
            .await?
        }
        None => Vec::new(),
    };
    Ok(PermissionSnapshot {
        database: session.info().database.clone(),
        supports_roles,
        literal_database_names,
        principals,
        memberships,
        grants,
        warnings,
    })
}

pub async fn apply(session: &dyn DatabaseSession, change: &PermissionChange) -> DbResult<()> {
    let engine = session.info().engine;
    ensure_supported(engine)?;
    if session.info().requested_access != AccessMode::ReadWrite {
        return Err(DatabaseError::new(
            ErrorKind::ReadOnly,
            "Permission changes require a writable administration session",
        ));
    }
    if session.transaction_state() != TransactionState::Idle {
        return Err(DatabaseError::invalid_request(
            "Permission changes require a separate session without an open transaction",
        ));
    }
    if change.requires_roles()
        && !supports_roles(
            engine,
            session.info().server_version.as_deref().unwrap_or(""),
        )
    {
        return Err(DatabaseError::new(
            ErrorKind::Unsupported,
            "This server version does not support roles",
        ));
    }
    if let PermissionChange::Privilege { scope, .. } = change {
        ensure_database(session, scope)?;
        ensure_routine(session, scope).await?;
    }
    change.preview(engine)?;
    let result = session.apply_permission_change(change).await;
    // Server diagnostics can echo part of a password-bearing statement.
    // Keep the code/category, but never return those diagnostics to the UI.
    if matches!(change, PermissionChange::CreateUser { .. }) {
        return result.map_err(|error| {
            let mut safe = DatabaseError::new(error.kind, "Could not create the user. Check account privileges, whether the account exists, and the server password policy.");
            safe.code = error.code;
            safe
        });
    }
    result
}

pub(crate) async fn execute_change<S: DatabaseSession + ?Sized>(
    session: &S,
    change: &PermissionChange,
) -> DbResult<()> {
    if session.info().requested_access != AccessMode::ReadWrite {
        return Err(DatabaseError::new(
            ErrorKind::ReadOnly,
            "Permission changes require a writable session",
        ));
    }
    collect(session, change.render(session.info().engine)?)
        .await
        .map(|_| ())
}

async fn catalog(
    session: &dyn DatabaseSession,
    sql: String,
    label: &str,
    warnings: &mut Vec<String>,
) -> DbResult<Vec<Row>> {
    match collect(session, sql).await {
        Ok(rows) => Ok(rows),
        Err(error) if error.kind == ErrorKind::Permission => {
            warnings.push(format!("{label} could not be listed: {}", error.message));
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}

async fn ensure_routine(session: &dyn DatabaseSession, scope: &PermissionScope) -> DbResult<()> {
    if session.info().engine != EngineKind::Postgres
        || !matches!(scope.kind, ScopeKind::Function | ScopeKind::Procedure)
    {
        return Ok(());
    }
    let rows = collect(session, super::discovery::routine_count_query(scope)?).await?;
    if rows.first().and_then(|row| row.first()) != Some(&Value::Integer(1)) {
        return Err(DatabaseError::invalid_request(
            "Enter an existing, unambiguous routine name. Manage overloaded PostgreSQL routines in the SQL console.",
        ));
    }
    Ok(())
}

fn ensure_database(session: &dyn DatabaseSession, scope: &PermissionScope) -> DbResult<()> {
    if session.info().engine == EngineKind::Postgres
        && scope.kind != ScopeKind::Database
        && session.info().database.as_deref() != Some(scope.database.as_str())
    {
        return Err(DatabaseError::invalid_request(
            "Open an administration session for the selected database",
        ));
    }
    Ok(())
}

async fn prepare(session: &dyn DatabaseSession) -> DbResult<()> {
    if matches!(
        session.info().engine,
        EngineKind::Mysql | EngineKind::Mariadb
    ) {
        collect(session, "SET SESSION sql_mode = CONCAT_WS(',', NULLIF(@@SESSION.sql_mode, ''), 'NO_BACKSLASH_ESCAPES')".to_owned()).await?;
    }
    Ok(())
}

fn text(row: &Row, index: usize) -> DbResult<String> {
    match row.get(index) {
        Some(Value::Text(value)) => Ok(value.clone()),
        _ => Err(DatabaseError::internal(
            "Unexpected permission catalog response",
        )),
    }
}

async fn collect<S: DatabaseSession + ?Sized>(session: &S, sql: String) -> DbResult<Vec<Row>> {
    let mut stream = session.execute(QueryRequest::new(sql))?;
    let mut rows = Vec::new();
    let mut failure = None;
    while let Some(event) = stream.events.recv().await {
        match event.kind {
            RequestEventKind::Rows { rows: batch, .. } => rows.extend(batch),
            RequestEventKind::StatementCompleted { summary, .. }
                if summary.truncation.is_some() =>
            {
                failure = Some(DatabaseError::new(
                    ErrorKind::InvalidRequest,
                    "The permission catalog exceeded the result limit; results would be incomplete",
                ));
            }
            RequestEventKind::Failed(error) => failure = Some(error),
            RequestEventKind::Cancelled => failure = Some(DatabaseError::cancelled()),
            RequestEventKind::Finished { .. } => return failure.map_or(Ok(rows), Err),
            _ => {}
        }
    }
    Err(failure.unwrap_or_else(|| {
        DatabaseError::internal("The administration statement ended without a result")
    }))
}
