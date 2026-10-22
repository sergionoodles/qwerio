//! Permission administration uses short-lived sessions, separate from every
//! query console and transaction. Every opened session is explicitly closed.

use std::sync::Arc;

use qwerio_core::error::DbResult;
use qwerio_core::permissions::{self, PermissionChange, PermissionScope, PermissionSnapshot};
use qwerio_core::session::AccessMode;

use crate::services::ResolvedConnection;
use crate::sessions::SessionRegistry;

pub async fn inspect_permissions(
    sessions: &Arc<SessionRegistry>,
    connection: ResolvedConnection,
    scope: Option<&PermissionScope>,
) -> DbResult<PermissionSnapshot> {
    permissions::ensure_supported(connection.descriptor.engine())?;
    let lease = sessions
        .open_console(
            connection.descriptor,
            connection.credentials,
            scope.and_then(PermissionScope::session_database),
            AccessMode::ReadOnly,
        )
        .await?;
    let result = permissions::inspect(lease.session().as_ref(), scope).await;
    let closed = lease.session().close().await;
    match result {
        Ok(snapshot) => closed.map(|()| snapshot),
        Err(error) => Err(error),
    }
}

pub async fn apply_permission_change(
    sessions: &Arc<SessionRegistry>,
    connection: ResolvedConnection,
    change: &PermissionChange,
) -> DbResult<()> {
    permissions::ensure_supported(connection.descriptor.engine())?;
    change.preview(connection.descriptor.engine())?;
    let lease = sessions
        .open_console(
            connection.descriptor,
            connection.credentials,
            change.session_database(),
            AccessMode::ReadWrite,
        )
        .await?;
    let result = permissions::apply(lease.session().as_ref(), change).await;
    let closed = lease.session().close().await;
    result.and(closed)
}
