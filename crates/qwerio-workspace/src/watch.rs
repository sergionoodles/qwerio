//! Watching the workspace folder for changes made by other programs.
//!
//! Events also fire for Qwerio's own saves; consumers compare fingerprints to
//! decide whether a change is external.

use std::path::{Path, PathBuf};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::document::{DocumentKind, TEMPORARY_MARKER};
use crate::error::{WorkspaceError, WorkspaceResult};
use crate::manifest::{MANIFEST_DIRECTORY, MANIFEST_FILE};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceChange {
    ManifestChanged,
    /// A document file was created or modified (workspace-relative path).
    DocumentChanged(PathBuf),
    DocumentRemoved(PathBuf),
    /// Watching lost events; the consumer should rescan.
    Rescan,
}

/// Keeps the platform watcher alive; dropping it stops watching.
pub struct WorkspaceWatcher {
    _watcher: RecommendedWatcher,
}

impl WorkspaceWatcher {
    /// `root` must be canonical. `on_change` runs on the watcher's thread.
    pub fn start(
        root: &Path,
        on_change: impl Fn(WorkspaceChange) + Send + 'static,
    ) -> WorkspaceResult<Self> {
        let watched_root = root.to_path_buf();

        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                let Ok(event) = result else {
                    on_change(WorkspaceChange::Rescan);
                    return;
                };

                if event.need_rescan() {
                    on_change(WorkspaceChange::Rescan);
                    return;
                }

                for path in &event.paths {
                    if let Some(change) = classify(&watched_root, path, &event.kind) {
                        on_change(change);
                    }
                }
            })
            .map_err(|error| watch_error(root, error))?;

        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| watch_error(root, error))?;

        Ok(Self { _watcher: watcher })
    }
}

fn watch_error(root: &Path, error: notify::Error) -> WorkspaceError {
    WorkspaceError::io(root, std::io::Error::other(error.to_string()))
}

/// Maps one event path to a workspace change, ignoring temporary save files,
/// unsupported files, and Qwerio's private folder except the manifest.
pub fn classify(root: &Path, path: &Path, kind: &EventKind) -> Option<WorkspaceChange> {
    let relative = path.strip_prefix(root).ok()?;
    let name = relative.file_name()?.to_string_lossy();

    if name.contains(TEMPORARY_MARKER) {
        return None;
    }

    if relative == Path::new(MANIFEST_DIRECTORY).join(MANIFEST_FILE) {
        return matches!(kind, EventKind::Create(_) | EventKind::Modify(_))
            .then_some(WorkspaceChange::ManifestChanged);
    }

    if relative.starts_with(MANIFEST_DIRECTORY) || DocumentKind::from_path(relative).is_none() {
        return None;
    }

    match kind {
        EventKind::Remove(_) => Some(WorkspaceChange::DocumentRemoved(relative.to_path_buf())),
        EventKind::Create(_) | EventKind::Modify(_) => {
            if path.exists() {
                Some(WorkspaceChange::DocumentChanged(relative.to_path_buf()))
            } else {
                // Renames report the old name as a modification.
                Some(WorkspaceChange::DocumentRemoved(relative.to_path_buf()))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use notify::event::{CreateKind, ModifyKind, RemoveKind};

    use super::*;

    #[test]
    fn classifies_document_and_manifest_changes() {
        let root = Path::new("/workspace");
        let modify = EventKind::Modify(ModifyKind::Any);

        assert_eq!(
            classify(
                root,
                Path::new("/workspace/.qwerio/workspace.toml"),
                &modify
            ),
            Some(WorkspaceChange::ManifestChanged)
        );
        assert_eq!(
            classify(
                root,
                Path::new("/workspace/q.sql"),
                &EventKind::Remove(RemoveKind::File)
            ),
            Some(WorkspaceChange::DocumentRemoved(PathBuf::from("q.sql")))
        );
    }

    #[test]
    fn ignores_temporary_and_unsupported_files() {
        let root = Path::new("/workspace");
        let create = EventKind::Create(CreateKind::File);

        assert_eq!(
            classify(
                root,
                Path::new("/workspace/.q.sql.qwerio-save-abc"),
                &create
            ),
            None
        );
        assert_eq!(
            classify(root, Path::new("/workspace/image.png"), &create),
            None
        );
        assert_eq!(classify(root, Path::new("/outside/q.sql"), &create), None);
    }
}
