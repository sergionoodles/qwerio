//! Application directories: XDG locations on Linux, the standard locations
//! on macOS.

use std::path::PathBuf;

/// Stable application ID, also used as the Wayland app ID and desktop entry.
pub const APPLICATION_ID: &str = "dev.qwerio.Qwerio";

const STATE_DATABASE: &str = "state.sqlite";

#[derive(Clone, Debug)]
pub struct AppPaths {
    /// Machine-local state (history, bindings, drafts).
    pub state_directory: PathBuf,
    pub data_directory: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Option<Self> {
        let directories = directories::ProjectDirs::from("dev", "qwerio", "Qwerio")?;
        let state_directory = directories
            .state_dir()
            .unwrap_or_else(|| directories.data_local_dir())
            .to_path_buf();

        Some(Self {
            state_directory,
            data_directory: directories.data_local_dir().to_path_buf(),
        })
    }

    pub fn state_database(&self) -> PathBuf {
        self.state_directory.join(STATE_DATABASE)
    }
}
