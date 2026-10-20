//! Blocking persistence operations for an open workspace. Views use the same
//! operations whether data lives in a portable file or legacy local storage.

mod connections;
mod documents;

use std::path::Path;
use std::sync::Arc;

use qwerio_core::connection::Secret;
use qwerio_workspace::{Workspace, WorkspaceResult};
use serde::{Serialize, de::DeserializeOwned};

use crate::{AppResult, AppServices};

pub use documents::SavedDocument;

pub struct WorkspaceStore {
    workspace: Workspace,
    services: Arc<AppServices>,
}

impl WorkspaceStore {
    pub fn open_for_import(
        path: &Path,
        password: Option<&Secret>,
        services: Arc<AppServices>,
    ) -> AppResult<Self> {
        Ok(Self::new(
            Workspace::open_read_only(path, password)?,
            services,
        ))
    }

    pub fn new(workspace: Workspace, services: Arc<AppServices>) -> Self {
        Self {
            workspace,
            services,
        }
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub fn update<T>(
        &mut self,
        operation: impl FnOnce(&mut Workspace) -> WorkspaceResult<T>,
    ) -> WorkspaceResult<T> {
        operation(&mut self.workspace)
    }

    pub fn ui_state<T: DeserializeOwned>(&self) -> AppResult<Option<T>> {
        if self.workspace.is_file_workspace() {
            return self
                .workspace
                .ui_state()
                .map(serde_json::from_str)
                .transpose()
                .map_err(Into::into);
        }
        self.services
            .local_state
            .ui_state(self.workspace.manifest().id)
    }

    pub fn save_ui_state(&mut self, state: &impl Serialize) -> AppResult<()> {
        if self.workspace.is_file_workspace() {
            self.workspace
                .save_ui_state(serde_json::to_string(state)?)?;
            return Ok(());
        }
        self.services
            .local_state
            .save_ui_state(self.workspace.manifest().id, state)
    }

    pub fn export(&self, path: &Path, password: Option<&Secret>) -> AppResult<()> {
        let connections = self.export_connections()?;
        let drafts = if self.workspace.is_file_workspace() {
            Vec::new()
        } else {
            self.services
                .local_state
                .drafts(self.workspace.manifest().id)?
                .into_iter()
                .map(|draft| (draft.document, draft.text))
                .collect()
        };
        let ui_state = if self.workspace.is_file_workspace() {
            None
        } else {
            self.ui_state::<serde_json::Value>()?
                .map(|state| serde_json::to_string(&state))
                .transpose()?
        };
        self.workspace
            .export_with_state(path, password, connections, drafts, ui_state)?;
        Ok(())
    }
}
