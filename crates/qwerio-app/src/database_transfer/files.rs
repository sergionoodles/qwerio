use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use qwerio_core::ids::ConnectionId;

/// Owns only a newly created private file. Never truncates user files.
pub(super) struct PrivateFile {
    pub path: PathBuf,
}

impl PrivateFile {
    pub fn create(directory: &Path) -> Result<(Self, File), String> {
        let path = directory.join(format!(".qwerio-transfer-{}", ConnectionId::new()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|error| error.to_string())?;
        Ok((Self { path }, file))
    }

    pub fn secret(contents: &str) -> Result<Self, String> {
        let (owned, mut file) = Self::create(&std::env::temp_dir())?;
        file.write_all(contents.as_bytes())
            .map_err(|error| error.to_string())?;
        Ok(owned)
    }

    pub fn publish(&self, destination: &Path) -> Result<(), String> {
        File::open(&self.path)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        // Linking is atomic and fails if the destination already exists.
        std::fs::hard_link(&self.path, destination).map_err(|error| error.to_string())
    }
}

impl Drop for PrivateFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            eprintln!("Could not remove transfer temporary file: {error}");
        }
    }
}
