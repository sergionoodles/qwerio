//! Portable `.qwerio` files containing workspace organization, documents, and
//! connection details, optionally encrypted. Legacy folder manifests remain
//! supported for opening and importing existing workspaces.

pub mod document;
pub mod error;
pub mod explorer;
pub mod manifest;
pub mod paths;
pub mod watch;
pub mod workspace;

pub use error::{WorkspaceError, WorkspaceResult};
pub use workspace::{MovableEntry, Workspace};

pub mod file;
pub mod labels;

mod file_workspace;
