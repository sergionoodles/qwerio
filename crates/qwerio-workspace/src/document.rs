//! Reading and atomically saving document files with external-change checks.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{WorkspaceError, WorkspaceResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DocumentKind {
    Sql,
    Markdown,
    Text,
}

impl DocumentKind {
    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();

        match extension.as_str() {
            "sql" => Some(Self::Sql),
            "md" | "markdown" => Some(Self::Markdown),
            "txt" => Some(Self::Text),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Sql => "sql",
            Self::Markdown => "md",
            Self::Text => "txt",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Sql => "SQL",
            Self::Markdown => "Markdown",
            Self::Text => "Text",
        }
    }
}

/// Identifies one version of a file's content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fingerprint {
    pub length: u64,
    /// FNV-1a of the content; stable across runs so persisted drafts can
    /// detect changes made while the application was closed.
    pub content_hash: u64,
}

impl Fingerprint {
    pub fn of(content: &[u8]) -> Self {
        Self {
            length: content.len() as u64,
            content_hash: fnv1a(content),
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    bytes.iter().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

/// A loaded document and the fingerprint it was loaded from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedDocument {
    pub text: String,
    pub fingerprint: Fingerprint,
    pub modified: Option<SystemTime>,
}

/// The current on-disk state compared with a loaded version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskState {
    Unchanged,
    Modified(Fingerprint),
    Deleted,
}

/// Reads a document. Invalid UTF-8 is rejected rather than altered.
pub fn read_document(path: &Path) -> WorkspaceResult<LoadedDocument> {
    let bytes = fs::read(path).map_err(|error| WorkspaceError::io(path, error))?;
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok();
    let fingerprint = Fingerprint::of(&bytes);
    let text = String::from_utf8(bytes).map_err(|_| {
        WorkspaceError::io(
            path,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the file is not valid UTF-8",
            ),
        )
    })?;

    Ok(LoadedDocument {
        text,
        fingerprint,
        modified,
    })
}

pub fn disk_state(path: &Path, loaded: Fingerprint) -> WorkspaceResult<DiskState> {
    match fs::read(path) {
        Ok(bytes) => {
            let current = Fingerprint::of(&bytes);

            if current == loaded {
                Ok(DiskState::Unchanged)
            } else {
                Ok(DiskState::Modified(current))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DiskState::Deleted),
        Err(error) => Err(WorkspaceError::io(path, error)),
    }
}

/// What a save may replace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveExpectation {
    /// The file must not exist yet.
    New,
    /// The file must still have this content; otherwise the save is a conflict.
    Unchanged(Fingerprint),
    /// Replace whatever is on disk (the user chose to overwrite).
    Overwrite,
}

/// Marker in temporary file names, used to clean up after interrupted saves.
pub const TEMPORARY_MARKER: &str = ".qwerio-save-";

/// Writes `text` to a temporary file in the same folder, flushes it to disk,
/// and renames it over the target, so readers see the old or the new content
/// and never a partial file.
pub fn save_document(
    path: &Path,
    text: &str,
    expectation: SaveExpectation,
) -> WorkspaceResult<Fingerprint> {
    save_bytes(path, text.as_bytes(), expectation)
}

pub(crate) fn save_bytes(
    path: &Path,
    bytes: &[u8],
    expectation: SaveExpectation,
) -> WorkspaceResult<Fingerprint> {
    check_expectation(path, expectation)?;

    let directory = path
        .parent()
        .ok_or_else(|| WorkspaceError::OutsideWorkspace(path.to_path_buf()))?;
    fs::create_dir_all(directory).map_err(|error| WorkspaceError::io(directory, error))?;

    let temporary = temporary_path(path);
    let result = write_and_sync(&temporary, bytes).and_then(|()| {
        if matches!(expectation, SaveExpectation::New) {
            fs::hard_link(&temporary, path).map_err(|error| WorkspaceError::io(path, error))?;
            fs::remove_file(&temporary).map_err(|error| WorkspaceError::io(&temporary, error))
        } else {
            fs::rename(&temporary, path).map_err(|error| WorkspaceError::io(path, error))
        }
    });

    if result.is_err() {
        // The original file is untouched; only the partial copy is removed.
        let _ = fs::remove_file(&temporary);
    }

    result?;
    sync_directory(directory);
    Ok(Fingerprint::of(bytes))
}

fn check_expectation(path: &Path, expectation: SaveExpectation) -> WorkspaceResult<()> {
    match expectation {
        SaveExpectation::Overwrite => Ok(()),
        SaveExpectation::New if path.symlink_metadata().is_ok() => {
            Err(WorkspaceError::AlreadyExists(path.to_path_buf()))
        }
        SaveExpectation::New => Ok(()),
        SaveExpectation::Unchanged(loaded) => match disk_state(path, loaded)? {
            DiskState::Unchanged => Ok(()),
            DiskState::Modified(on_disk) => Err(WorkspaceError::Conflict {
                path: path.to_path_buf(),
                on_disk: Some(on_disk),
            }),
            DiskState::Deleted => Err(WorkspaceError::Conflict {
                path: path.to_path_buf(),
                on_disk: None,
            }),
        },
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let unique = uuid::Uuid::new_v4().simple();

    path.with_file_name(format!(".{name}{TEMPORARY_MARKER}{unique}"))
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> WorkspaceResult<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| WorkspaceError::io(path, error))?;

    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| WorkspaceError::io(path, error))
}

/// Persists the rename itself. Not supported on every platform; failure only
/// weakens durability after a power loss, so it is not reported.
fn sync_directory(directory: &Path) {
    if let Ok(handle) = File::open(directory) {
        let _ = handle.sync_all();
    }
}

/// Removes temporary files left by saves interrupted by a crash. Returns the
/// removed paths. Original documents are never touched.
pub fn remove_interrupted_saves(directory: &Path) -> WorkspaceResult<Vec<PathBuf>> {
    let mut removed = Vec::new();
    let entries = fs::read_dir(directory).map_err(|error| WorkspaceError::io(directory, error))?;

    for entry in entries {
        let entry = entry.map_err(|error| WorkspaceError::io(directory, error))?;
        let path = entry.path();
        let is_temporary = entry
            .file_name()
            .to_string_lossy()
            .contains(TEMPORARY_MARKER);

        if is_temporary && path.is_file() {
            fs::remove_file(&path).map_err(|error| WorkspaceError::io(&path, error))?;
            removed.push(path);
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_new_files_and_refuses_to_replace_existing_ones() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("q.sql");

        save_document(&path, "SELECT 1", SaveExpectation::New).expect("first save");
        let second = save_document(&path, "SELECT 2", SaveExpectation::New);

        assert!(matches!(second, Err(WorkspaceError::AlreadyExists(_))));
        assert_eq!(fs::read_to_string(&path).ok().as_deref(), Some("SELECT 1"));
    }

    #[test]
    fn external_changes_turn_saves_into_conflicts() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("notes.md");
        let loaded = save_document(&path, "# Draft", SaveExpectation::New).expect("save");

        fs::write(&path, "# Edited elsewhere").expect("external edit");
        let result = save_document(&path, "# Mine", SaveExpectation::Unchanged(loaded));

        assert!(matches!(
            result,
            Err(WorkspaceError::Conflict {
                on_disk: Some(_),
                ..
            })
        ));
        assert_eq!(
            fs::read_to_string(&path).ok().as_deref(),
            Some("# Edited elsewhere")
        );
    }

    #[test]
    fn deleted_files_are_reported_as_conflicts() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("gone.txt");
        let loaded = save_document(&path, "x", SaveExpectation::New).expect("save");
        fs::remove_file(&path).expect("delete");

        let result = save_document(&path, "y", SaveExpectation::Unchanged(loaded));

        assert!(matches!(
            result,
            Err(WorkspaceError::Conflict { on_disk: None, .. })
        ));
    }

    #[test]
    fn unchanged_files_save_and_return_the_new_fingerprint() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("q.sql");
        let loaded = save_document(&path, "SELECT 1", SaveExpectation::New).expect("save");

        let saved =
            save_document(&path, "SELECT 2", SaveExpectation::Unchanged(loaded)).expect("resave");

        assert_eq!(saved, Fingerprint::of(b"SELECT 2"));
        assert_eq!(
            read_document(&path)
                .map(|document| document.text)
                .ok()
                .as_deref(),
            Some("SELECT 2")
        );
    }

    #[test]
    fn interrupted_saves_leave_the_original_and_are_cleaned_up() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("q.sql");
        save_document(&path, "SELECT 1", SaveExpectation::New).expect("save");

        // A crash between writing the temporary file and renaming it.
        let leftover = temporary_path(&path);
        fs::write(&leftover, "SELECT partial").expect("leftover");

        let removed = remove_interrupted_saves(directory.path()).expect("cleanup");

        assert_eq!(removed, vec![leftover]);
        assert_eq!(fs::read_to_string(&path).ok().as_deref(), Some("SELECT 1"));
    }

    #[test]
    fn invalid_utf8_is_rejected_rather_than_altered() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let path = directory.path().join("binary.txt");
        fs::write(&path, [0xff, 0xfe]).expect("write");

        assert!(read_document(&path).is_err());
    }

    #[test]
    fn kinds_follow_extensions() {
        assert_eq!(
            DocumentKind::from_path(Path::new("a.SQL")),
            Some(DocumentKind::Sql)
        );
        assert_eq!(
            DocumentKind::from_path(Path::new("a.md")),
            Some(DocumentKind::Markdown)
        );
        assert_eq!(DocumentKind::from_path(Path::new("a.csv")), None);
    }
}
