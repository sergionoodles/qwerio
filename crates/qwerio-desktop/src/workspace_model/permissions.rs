use super::*;
use qwerio_core::permissions::{PermissionChange, PermissionScope, PermissionSnapshot};

impl WorkspaceModel {
    pub fn inspect_permissions(
        &self,
        connection: ConnectionId,
        scope: Option<PermissionScope>,
        cx: &App,
    ) -> Task<Result<PermissionSnapshot, SharedString>> {
        let resolve = self.resolve(connection, cx);
        let sessions = Arc::clone(&self.services.sessions);
        cx.spawn(async move |_| {
            let resolved = resolve.await?;
            qwerio_app::permissions::inspect_permissions(&sessions, resolved, scope.as_ref())
                .await
                .map_err(|error| error_text(&AppError::Database(error)))
        })
    }

    pub fn apply_permission_change(
        &self,
        connection: ConnectionId,
        change: PermissionChange,
        cx: &App,
    ) -> Task<Result<(), SharedString>> {
        let resolve = self.resolve(connection, cx);
        let sessions = Arc::clone(&self.services.sessions);
        cx.spawn(async move |_| {
            let resolved = resolve.await?;
            qwerio_app::permissions::apply_permission_change(&sessions, resolved, &change)
                .await
                .map_err(|error| error_text(&AppError::Database(error)))
        })
    }
}
