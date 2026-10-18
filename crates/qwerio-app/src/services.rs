//! The application service facade shared by the desktop views.

use std::io::Write;
use std::sync::Arc;

use qwerio_core::connection::{ConnectionDescriptor, Credentials, Secret};
use qwerio_core::driver::DatabaseSession;
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::export::{ExportFormat, write_header, write_rows};
use qwerio_core::ids::ConnectionId;
use qwerio_core::paging::PageRequest;
use qwerio_workspace::manifest::Manifest;

use crate::credentials::{CredentialRef, CredentialStore};
use crate::error::{AppError, AppResult};
use crate::local_state::{ConnectionBinding, LocalState};
use crate::registry::DriverRegistry;
use crate::sessions::SessionRegistry;

/// Rows a full export writes at most.
pub const MAX_EXPORT_ROWS: u64 = 1_000_000;

pub struct AppServices {
    pub drivers: Arc<DriverRegistry>,
    pub local_state: Arc<LocalState>,
    pub credentials: Arc<dyn CredentialStore>,
    pub sessions: Arc<SessionRegistry>,
}

/// A connection ready to open: its descriptor and resolved credentials.
#[derive(Clone, Debug)]
pub struct ResolvedConnection {
    pub descriptor: ConnectionDescriptor,
    pub credentials: Credentials,
}

impl AppServices {
    pub fn new(
        drivers: DriverRegistry,
        local_state: LocalState,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        let drivers = Arc::new(drivers);

        Self {
            sessions: Arc::new(SessionRegistry::new(Arc::clone(&drivers))),
            drivers,
            local_state: Arc::new(local_state),
            credentials,
        }
    }

    /// Combines the shared alias with this machine's binding and secret.
    /// Blocking (local state and keychain); run off the UI thread.
    pub fn resolve_connection(
        &self,
        manifest: &Manifest,
        connection: ConnectionId,
    ) -> AppResult<ResolvedConnection> {
        let alias = manifest
            .connection(connection)
            .ok_or(AppError::UnknownConnection(connection))?;
        let binding = self
            .local_state
            .binding(manifest.id, connection)?
            .ok_or(AppError::UnboundConnection(connection))?;

        if binding.options.engine() != alias.engine {
            return Err(AppError::UnboundConnection(connection));
        }

        let password = match &binding.credential {
            Some(reference) => self.credentials.load(reference)?,
            None => None,
        };

        Ok(ResolvedConnection {
            descriptor: ConnectionDescriptor {
                id: connection,
                name: alias.name.clone(),
                options: binding.options,
            },
            credentials: Credentials { password },
        })
    }

    /// Saves this machine's endpoint for an alias and, when given, its
    /// password in the credential store. Blocking.
    pub fn bind_connection(
        &self,
        manifest: &Manifest,
        connection: ConnectionId,
        options: qwerio_core::connection::ConnectionOptions,
        password: Option<Secret>,
    ) -> AppResult<()> {
        let reference = CredentialRef::for_connection(manifest.id, connection);

        let credential = match password {
            Some(secret) => {
                self.credentials.store(&reference, &secret)?;
                Some(reference)
            }
            None => {
                self.credentials.delete(&reference)?;
                None
            }
        };

        self.local_state.save_binding(
            manifest.id,
            connection,
            &ConnectionBinding {
                options,
                credential,
            },
        )
    }

    /// Removes this machine's binding and stored secret for an alias.
    pub fn unbind_connection(
        &self,
        manifest: &Manifest,
        connection: ConnectionId,
    ) -> AppResult<()> {
        self.credentials
            .delete(&CredentialRef::for_connection(manifest.id, connection))?;
        self.local_state.remove_binding(manifest.id, connection)
    }
}

/// Exports every row matching the request (not only the loaded rows) by
/// paging through the table, up to [`MAX_EXPORT_ROWS`].
pub async fn export_table(
    session: &dyn DatabaseSession,
    mut request: PageRequest,
    format: ExportFormat,
    writer: &mut (dyn Write + Send),
) -> DbResult<ExportSummary> {
    let mut written = 0_u64;
    let mut wrote_header = false;

    loop {
        let page = session.fetch_page(request.clone()).await?;

        if !wrote_header {
            write_header(writer, format, &page.columns).map_err(export_error)?;
            wrote_header = true;
        }

        let remaining = usize::try_from(MAX_EXPORT_ROWS - written).unwrap_or(usize::MAX);
        let rows = &page.rows[..page.rows.len().min(remaining)];
        write_rows(writer, format, rows).map_err(export_error)?;
        written += rows.len() as u64;

        let has_more = page.next.is_some() || rows.len() < page.rows.len();

        if written >= MAX_EXPORT_ROWS || !has_more {
            return Ok(ExportSummary {
                rows: written,
                hit_limit: has_more,
            });
        }

        request.continuation = page.next;
    }
}

fn export_error(error: std::io::Error) -> DatabaseError {
    DatabaseError::new(ErrorKind::Other, format!("Export failed: {error}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportSummary {
    pub rows: u64,
    pub hit_limit: bool,
}
