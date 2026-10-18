//! Workspaces: a user-selected folder of ordinary `.sql`, `.md`, and `.txt`
//! files plus a versioned manifest that places documents beside database
//! objects, associates them with objects, and records execution bindings.
//!
//! Virtual database entries never exist as directories on disk, and moving an
//! entry in the explorer changes organization only.

pub mod document;
pub mod error;
pub mod explorer;
pub mod manifest;
pub mod paths;
pub mod watch;
pub mod workspace;

pub use error::{WorkspaceError, WorkspaceResult};
pub use workspace::{MovableEntry, Workspace};
