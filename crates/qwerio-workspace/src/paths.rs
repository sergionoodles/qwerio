//! Confining document paths to the workspace folder.
//!
//! Relative paths must not be absolute or contain `..`, and their existing
//! ancestors must resolve (following symlinks) inside the canonical root.

use std::path::{Component, Path, PathBuf};

use crate::error::{WorkspaceError, WorkspaceResult};

/// Resolves a workspace-relative path to an absolute path inside `root`.
/// `root` must already be canonical.
pub fn resolve_inside(root: &Path, relative: &Path) -> WorkspaceResult<PathBuf> {
    if !is_plain_relative(relative) {
        return Err(WorkspaceError::OutsideWorkspace(relative.to_path_buf()));
    }

    let candidate = root.join(relative);
    let existing = deepest_existing_ancestor(&candidate);
    let canonical = existing
        .canonicalize()
        .map_err(|error| WorkspaceError::io(existing, error))?;

    if !canonical.starts_with(root) {
        return Err(WorkspaceError::OutsideWorkspace(relative.to_path_buf()));
    }

    Ok(candidate)
}

/// The workspace-relative form of an absolute path inside `root`.
pub fn relative_to(root: &Path, path: &Path) -> WorkspaceResult<PathBuf> {
    let canonical_path = match path.canonicalize() {
        Ok(canonical) => canonical,
        Err(_) => path.to_path_buf(),
    };

    canonical_path
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|_| WorkspaceError::OutsideWorkspace(path.to_path_buf()))
}

pub(crate) fn is_plain_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn deepest_existing_ancestor(path: &Path) -> &Path {
    path.ancestors()
        .find(|ancestor| ancestor.symlink_metadata().is_ok())
        .unwrap_or(path)
}

/// Uses `/` separators so manifests are portable between platforms.
pub fn portable(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub fn from_portable(text: &str) -> PathBuf {
    text.split('/').filter(|part| !part.is_empty()).collect()
}

/// Checks a single file or folder name chosen by the user.
pub fn validate_name(name: &str) -> WorkspaceResult<()> {
    let trimmed = name.trim();
    let has_separator = trimmed.contains(['/', '\\']);
    let is_reserved = matches!(trimmed, "" | "." | "..");
    let has_control = trimmed.chars().any(char::is_control);

    if has_separator || is_reserved || has_control || trimmed != name {
        return Err(WorkspaceError::InvalidName(name.to_owned()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_temp() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let root = directory.path().canonicalize().expect("canonical root");
        (directory, root)
    }

    #[test]
    fn rejects_parent_and_absolute_paths() {
        let (_directory, root) = canonical_temp();

        assert!(resolve_inside(&root, Path::new("../escape.sql")).is_err());
        assert!(resolve_inside(&root, Path::new("a/../../escape.sql")).is_err());
        assert!(resolve_inside(&root, &root.join("absolute.sql")).is_err());
        assert!(resolve_inside(&root, Path::new("")).is_err());
    }

    #[test]
    fn accepts_nested_paths_that_do_not_exist_yet() {
        let (_directory, root) = canonical_temp();
        let resolved = resolve_inside(&root, Path::new("queries/new/report.sql"));

        assert_eq!(resolved.ok(), Some(root.join("queries/new/report.sql")));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_leave_the_workspace() {
        let (_directory, root) = canonical_temp();
        let (_outside_directory, outside) = canonical_temp();
        std::os::unix::fs::symlink(&outside, root.join("link")).expect("symlink");

        assert!(resolve_inside(&root, Path::new("link/secret.sql")).is_err());
    }

    #[test]
    fn portable_paths_use_forward_slashes() {
        let path: PathBuf = ["queries", "daily", "top.sql"].iter().collect();

        assert_eq!(portable(&path), "queries/daily/top.sql");
        assert_eq!(from_portable("queries/daily/top.sql"), path);
    }

    #[test]
    fn names_cannot_contain_separators_or_be_reserved() {
        assert!(validate_name("report.sql").is_ok());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name(" padded").is_err());
    }
}
