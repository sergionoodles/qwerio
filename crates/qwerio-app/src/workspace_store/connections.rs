use std::collections::HashSet;

use qwerio_core::connection::{ConnectionDescriptor, ConnectionOptions, Credentials, Secret};
use qwerio_core::ids::ConnectionId;
use qwerio_workspace::file::StoredConnection;

use super::WorkspaceStore;
use crate::services::ResolvedConnection;
use crate::{AppError, AppResult};

impl WorkspaceStore {
    pub fn bound_connections(&self) -> AppResult<HashSet<ConnectionId>> {
        let mut bound: HashSet<_> = self
            .services
            .local_state
            .bound_connections(self.workspace.manifest().id)?
            .into_iter()
            .collect();
        bound.extend(
            self.workspace
                .manifest()
                .connections
                .iter()
                .filter(|alias| self.workspace.stored_connection(alias.id).is_some())
                .map(|alias| alias.id),
        );
        Ok(bound)
    }

    pub fn resolve_connection(&self, connection: ConnectionId) -> AppResult<ResolvedConnection> {
        if let Some(stored) = self.workspace.stored_connection(connection) {
            let alias = self
                .workspace
                .manifest()
                .connection(connection)
                .ok_or(AppError::UnknownConnection(connection))?;
            return Ok(ResolvedConnection {
                descriptor: ConnectionDescriptor {
                    id: connection,
                    name: alias.name.clone(),
                    options: stored.options.clone(),
                },
                credentials: Credentials {
                    password: stored.password.clone(),
                },
            });
        }
        self.services
            .resolve_connection(self.workspace.manifest(), connection)
    }

    pub fn bind_connection(
        &mut self,
        connection: ConnectionId,
        options: ConnectionOptions,
        password: Option<Secret>,
    ) -> AppResult<()> {
        if self.workspace.is_file_workspace() {
            self.workspace.save_connection(StoredConnection {
                id: connection,
                options,
                password,
            })?;
            return Ok(());
        }
        self.services
            .bind_connection(self.workspace.manifest(), connection, options, password)
    }

    pub fn stored_password(&self, connection: ConnectionId) -> AppResult<Option<Secret>> {
        if self.workspace.is_file_workspace() {
            return Ok(self
                .workspace
                .stored_connection(connection)
                .and_then(|stored| stored.password.clone()));
        }
        let reference = crate::credentials::CredentialRef::for_connection(
            self.workspace.manifest().id,
            connection,
        );
        self.services.credentials.load(&reference)
    }

    pub(super) fn export_connections(&self) -> AppResult<Vec<StoredConnection>> {
        let mut connections = Vec::new();
        for alias in &self.workspace.manifest().connections {
            if let Some(stored) = self.workspace.stored_connection(alias.id) {
                connections.push(stored.clone());
                continue;
            }
            let Some(binding) = self
                .services
                .local_state
                .binding(self.workspace.manifest().id, alias.id)?
            else {
                continue;
            };
            let resolved = self.resolve_connection(alias.id)?;
            if binding.credential.is_some() && resolved.credentials.password.is_none() {
                return Err(AppError::Credentials("A saved database password is unavailable in the keyring. Restore access before exporting.".into()));
            }
            connections.push(StoredConnection {
                id: alias.id,
                options: resolved.descriptor.options,
                password: resolved.credentials.password,
            });
        }
        Ok(connections)
    }
}
