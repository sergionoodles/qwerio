use qwerio_core::error::DatabaseError;
use qwerio_core::ids::ConnectionId;
use qwerio_workspace::WorkspaceError;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Database(#[from] DatabaseError),

    #[error(transparent)]
    Workspace(#[from] WorkspaceError),

    #[error("Local application state: {0}")]
    LocalState(String),

    #[error("Credential storage: {0}")]
    Credentials(String),

    #[error("The connection is not part of this workspace")]
    UnknownConnection(ConnectionId),

    /// The alias exists but has no endpoint or file on this machine.
    #[error("The connection has no address or file on this computer yet")]
    UnboundConnection(ConnectionId),

    #[error("No driver is registered for {0}")]
    MissingDriver(qwerio_core::connection::EngineKind),
}

impl From<rusqlite::Error> for AppError {
    fn from(error: rusqlite::Error) -> Self {
        Self::LocalState(error.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(error: serde_json::Error) -> Self {
        Self::LocalState(error.to_string())
    }
}
