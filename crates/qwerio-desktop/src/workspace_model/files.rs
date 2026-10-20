//! File operations and document I/O shared by the views.
use super::*;
use qwerio_core::connection::{ConnectionOptions, Secret};

impl WorkspaceModel {
    pub fn is_file_workspace(&self) -> bool {
        self.is_file_workspace
    }
    pub fn is_encrypted(&self) -> bool {
        self.is_encrypted
    }
    pub fn directory(&self) -> PathBuf {
        if self.is_file_workspace {
            return self
                .root
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(std::env::temp_dir);
        }
        self.root.clone()
    }

    pub fn storage(&self) -> Arc<Mutex<WorkspaceStore>> {
        Arc::clone(&self.storage)
    }

    pub fn bind_connection(
        &mut self,
        connection: ConnectionId,
        options: ConnectionOptions,
        password: Option<Secret>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), SharedString>> {
        let storage = self.storage();
        cx.background_spawn(async move {
            storage
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .bind_connection(connection, options, password)
                .map_err(|error| error_text(&error))
        })
    }

    pub fn record_recent(&self, cx: &App) -> Task<Result<(), SharedString>> {
        let local = Arc::clone(&self.services.local_state);
        let path = self.root.clone();
        let name = if self.is_encrypted {
            "Encrypted Workspace".to_owned()
        } else {
            self.manifest.name.clone()
        };
        cx.background_spawn(async move {
            local
                .record_recent_workspace(&path, &name, std::time::SystemTime::now())
                .map_err(|error| error_text(&error))
        })
    }

    pub fn export(
        &self,
        path: PathBuf,
        password: Option<Secret>,
        cx: &App,
    ) -> Task<Result<(), SharedString>> {
        let storage = self.storage();
        cx.background_spawn(async move {
            storage
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .export(&path, password.as_ref())
                .map_err(|error| error_text(&error))
        })
    }
}
