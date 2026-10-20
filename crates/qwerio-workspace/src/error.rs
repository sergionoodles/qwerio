use std::path::PathBuf;

use crate::document::Fingerprint;

pub type WorkspaceResult<T> = Result<T, WorkspaceError>;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("This workspace requires a password")]
    PasswordRequired,
    #[error("The password is incorrect or the workspace is damaged")]
    Authentication,
    #[error("The workspace file is invalid or exceeds the supported size")]
    InvalidFile,
    #[error("Secure encryption could not be initialized")]
    Encryption,

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("The workspace manifest is invalid: {0}")]
    InvalidManifest(String),

    #[error(
        "The workspace was saved by a newer version of Qwerio (manifest version {found}, supported {supported})"
    )]
    NewerManifest { found: u32, supported: u32 },

    #[error("{0} is outside the workspace folder")]
    OutsideWorkspace(PathBuf),

    #[error("{0} is not a supported document type; use .sql, .md, or .txt")]
    UnsupportedDocument(PathBuf),

    #[error("{0} already exists")]
    AlreadyExists(PathBuf),

    #[error("No {kind} with that identifier exists in the workspace")]
    NotFound { kind: &'static str },

    #[error("\"{0}\" is not a valid name")]
    InvalidName(String),

    #[error("A folder cannot be moved into itself")]
    FolderCycle,

    /// The file changed on disk since it was loaded. Nothing was written.
    #[error("{path} was changed by another program")]
    Conflict {
        path: PathBuf,
        on_disk: Option<Fingerprint>,
    },
}

impl WorkspaceError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
