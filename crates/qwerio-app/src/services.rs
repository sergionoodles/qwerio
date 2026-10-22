//! The application service facade shared by the desktop views.

use std::sync::Arc;

use qwerio_core::connection::{ConnectionDescriptor, Credentials, Secret};
use qwerio_core::ids::ConnectionId;
use qwerio_workspace::manifest::Manifest;

use crate::credentials::{CredentialRef, CredentialStore};
use crate::error::{AppError, AppResult};
use crate::local_state::{ConnectionBinding, LocalState};
use crate::registry::DriverRegistry;
use crate::sessions::SessionRegistry;

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
