//! An open workspace file, or a legacy folder and its manifest.
//!
//! Every document path is confined to the folder. Manifest changes are saved
//! atomically and refuse to overwrite a manifest changed by another program.

use std::fs;
use std::path::{Path, PathBuf};

use qwerio_core::connection::EngineKind;
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::ObjectRef;

use crate::document::{
    DocumentKind, Fingerprint, LoadedDocument, SaveExpectation, read_document,
    remove_interrupted_saves, save_document,
};
use crate::error::{WorkspaceError, WorkspaceResult};
use crate::manifest::{
    ConnectionAlias, DocumentEntry, DocumentId, ExecutionBinding, FolderEntry, FolderId,
    MANIFEST_DIRECTORY, MANIFEST_FILE, Manifest, Placement,
};
use crate::paths::{from_portable, portable, relative_to, resolve_inside, validate_name};

/// Folders skipped when looking for untracked documents.
const SKIPPED_DIRECTORIES: [&str; 3] = ["target", "node_modules", "build"];

/// Bounds the untracked-document scan in very large folders.
const MAX_SCANNED_ENTRIES: usize = 20_000;
const MAX_SCAN_DEPTH: usize = 8;

#[derive(Debug)]
pub struct Workspace {
    pub(crate) root: PathBuf,
    pub(crate) manifest: Manifest,
    pub(crate) manifest_fingerprint: Fingerprint,
    pub(crate) file: Option<crate::file::WorkspaceFile>,
}

/// An explorer entry that can be moved between placements.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MovableEntry {
    Document(DocumentId),
    Folder(FolderId),
}

impl Workspace {
    /// Creates a manifest in an existing folder.
    pub fn create(root: &Path, name: &str) -> WorkspaceResult<Self> {
        let root = canonical_directory(root)?;
        let manifest = Manifest::new(name);
        let path = manifest_path(&root);
        let text = manifest.to_toml()?;
        let manifest_fingerprint = save_document(&path, &text, SaveExpectation::New)?;

        Ok(Self {
            root,
            manifest,
            manifest_fingerprint,
            file: None,
        })
    }

    pub fn open(root: &Path) -> WorkspaceResult<Self> {
        let workspace = Self::open_read_only(root, None)?;
        if !workspace.is_file_workspace() {
            remove_interrupted_saves(&workspace.root.join(MANIFEST_DIRECTORY))?;
        }
        Ok(workspace)
    }

    /// Reads an import source without changing it or cleaning temporary files.
    pub fn open_read_only(
        root: &Path,
        password: Option<&qwerio_core::connection::Secret>,
    ) -> WorkspaceResult<Self> {
        if root.is_file() {
            return Self::open_file(root, password);
        }
        let root = canonical_directory(root)?;
        let path = manifest_path(&root);
        let loaded = read_document(&path)?;
        let manifest = Manifest::parse(&loaded.text)?;

        Ok(Self {
            root,
            manifest,
            manifest_fingerprint: loaded.fingerprint,
            file: None,
        })
    }

    pub fn is_workspace(root: &Path) -> bool {
        root.extension()
            .is_some_and(|extension| extension == "qwerio")
            || manifest_path(root).is_file()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.file
            .as_ref()
            .map_or_else(|| manifest_path(&self.root), |file| file.path.clone())
    }

    /// Re-reads the manifest after another program changed it.
    pub fn reload_manifest(&mut self) -> WorkspaceResult<()> {
        if let Some(file) = &mut self.file {
            file.reload()?;
            self.manifest = file.contents.manifest.clone();
            return Ok(());
        }
        let loaded = read_document(&self.manifest_path())?;
        self.manifest = Manifest::parse(&loaded.text)?;
        self.manifest_fingerprint = loaded.fingerprint;
        Ok(())
    }

    /// Applies a change and saves it; on failure the in-memory manifest is
    /// left unchanged.
    pub(crate) fn update_manifest<T>(
        &mut self,
        change: impl FnOnce(&mut Manifest) -> WorkspaceResult<T>,
    ) -> WorkspaceResult<T> {
        let mut updated = self.manifest.clone();
        let output = change(&mut updated)?;
        updated
            .label_assignments
            .retain(|assignment| match assignment.target {
                crate::labels::LabelTarget::Document { document } => {
                    updated.documents.iter().any(|entry| entry.id == document)
                }
                _ => true,
            });
        if let Some(file) = &mut self.file {
            let mut contents = file.contents.clone();
            contents.manifest = updated.clone();
            contents
                .connections
                .retain(|connection| updated.connection(connection.id).is_some());
            contents.documents.retain(|id, _| {
                updated
                    .documents
                    .iter()
                    .any(|document| document.id.to_string() == *id)
            });
            contents
                .drafts
                .retain(|id, _| contents.documents.contains_key(id));
            file.save(contents)?;
            self.manifest = updated;
            return Ok(output);
        }
        let text = updated.to_toml()?;
        let fingerprint = save_document(
            &self.manifest_path(),
            &text,
            SaveExpectation::Unchanged(self.manifest_fingerprint),
        )?;

        self.manifest = updated;
        self.manifest_fingerprint = fingerprint;
        Ok(output)
    }

    pub fn rename_workspace(&mut self, name: &str) -> WorkspaceResult<()> {
        let name = name.trim().to_owned();

        if name.is_empty() {
            return Err(WorkspaceError::InvalidName(name));
        }

        self.update_manifest(|manifest| {
            manifest.name = name;
            Ok(())
        })
    }

    // --- Connections ---------------------------------------------------------

    pub fn add_connection(
        &mut self,
        name: &str,
        engine: EngineKind,
    ) -> WorkspaceResult<ConnectionId> {
        let id = ConnectionId::new();
        let alias = ConnectionAlias {
            id,
            name: name.trim().to_owned(),
            engine,
        };

        self.update_manifest(|manifest| {
            manifest.connections.push(alias);
            Ok(id)
        })
    }

    pub fn rename_connection(&mut self, id: ConnectionId, name: &str) -> WorkspaceResult<()> {
        let name = name.trim().to_owned();

        self.update_manifest(|manifest| {
            let connection = manifest
                .connections
                .iter_mut()
                .find(|connection| connection.id == id)
                .ok_or(WorkspaceError::NotFound { kind: "connection" })?;
            connection.name = name;
            Ok(())
        })
    }

    /// Removes the alias. Documents keep their associations and bindings,
    /// which become unresolved and can be repaired later.
    pub fn remove_connection(&mut self, id: ConnectionId) -> WorkspaceResult<()> {
        self.update_manifest(|manifest| {
            let before = manifest.connections.len();
            manifest.connections.retain(|connection| connection.id != id);

            if manifest.connections.len() == before {
                return Err(WorkspaceError::NotFound { kind: "connection" });
            }

            for entry in &mut manifest.folders {
                if matches!(&entry.placement, Placement::Container { connection, .. } if *connection == id) {
                    entry.placement = Placement::Root;
                }
            }

            for entry in &mut manifest.documents {
                if matches!(&entry.placement, Placement::Container { connection, .. } if *connection == id) {
                    entry.placement = Placement::Root;
                }
            }

            Ok(())
        })
    }

    // --- Folders -------------------------------------------------------------

    pub fn create_folder(&mut self, name: &str, placement: Placement) -> WorkspaceResult<FolderId> {
        validate_name(name)?;
        self.ensure_placement_exists(&placement)?;
        let id = FolderId::new();
        let folder = FolderEntry {
            id,
            name: name.to_owned(),
            placement,
        };

        self.update_manifest(|manifest| {
            manifest.folders.push(folder);
            Ok(id)
        })
    }

    pub fn rename_folder(&mut self, id: FolderId, name: &str) -> WorkspaceResult<()> {
        validate_name(name)?;
        let name = name.to_owned();

        self.update_manifest(|manifest| {
            let folder = manifest
                .folders
                .iter_mut()
                .find(|folder| folder.id == id)
                .ok_or(WorkspaceError::NotFound { kind: "folder" })?;
            folder.name = name;
            Ok(())
        })
    }

    /// Removes a virtual folder. Its contents move to the folder's own placement.
    pub fn remove_folder(&mut self, id: FolderId) -> WorkspaceResult<()> {
        self.update_manifest(|manifest| {
            let position = manifest
                .folders
                .iter()
                .position(|folder| folder.id == id)
                .ok_or(WorkspaceError::NotFound { kind: "folder" })?;
            let removed = manifest.folders.remove(position);
            let inside = Placement::Folder { folder: id };

            for folder in &mut manifest.folders {
                if folder.placement == inside {
                    folder.placement = removed.placement.clone();
                }
            }

            for document in &mut manifest.documents {
                if document.placement == inside {
                    document.placement = removed.placement.clone();
                }
            }

            Ok(())
        })
    }

    fn ensure_placement_exists(&self, placement: &Placement) -> WorkspaceResult<()> {
        match placement {
            Placement::Root => Ok(()),
            Placement::Folder { folder } => self
                .manifest
                .folder(*folder)
                .map(|_| ())
                .ok_or(WorkspaceError::NotFound { kind: "folder" }),
            Placement::Container { connection, .. } => self
                .manifest
                .connection(*connection)
                .map(|_| ())
                .ok_or(WorkspaceError::NotFound { kind: "connection" }),
        }
    }

    /// Changes where an entry appears. Never moves files or database objects.
    pub fn move_entry(&mut self, entry: MovableEntry, placement: Placement) -> WorkspaceResult<()> {
        self.ensure_placement_exists(&placement)?;

        if let (MovableEntry::Folder(folder), Placement::Folder { folder: target }) =
            (entry, &placement)
            && self.folder_contains(folder, *target)
        {
            return Err(WorkspaceError::FolderCycle);
        }

        self.update_manifest(|manifest| {
            let current = match entry {
                MovableEntry::Document(id) => manifest
                    .documents
                    .iter_mut()
                    .find(|document| document.id == id)
                    .map(|document| &mut document.placement),
                MovableEntry::Folder(id) => manifest
                    .folders
                    .iter_mut()
                    .find(|folder| folder.id == id)
                    .map(|folder| &mut folder.placement),
            };

            *current.ok_or(WorkspaceError::NotFound { kind: "entry" })? = placement;
            Ok(())
        })
    }

    /// Whether `candidate` is `ancestor` or nested anywhere inside it.
    fn folder_contains(&self, ancestor: FolderId, candidate: FolderId) -> bool {
        let mut current = Some(candidate);
        let mut steps = 0;

        while let Some(folder) = current {
            if folder == ancestor {
                return true;
            }

            steps += 1;

            if steps > self.manifest.folders.len() {
                return true;
            }

            current = match self.manifest.folder(folder).map(|entry| &entry.placement) {
                Some(Placement::Folder { folder }) => Some(*folder),
                _ => None,
            };
        }

        false
    }

    // --- Documents -----------------------------------------------------------

    pub fn document_path(&self, id: DocumentId) -> WorkspaceResult<PathBuf> {
        let document = self
            .manifest
            .document(id)
            .ok_or(WorkspaceError::NotFound { kind: "document" })?;

        resolve_inside(&self.root, &from_portable(&document.path))
    }

    /// Creates a new file and registers it.
    pub fn create_document(
        &mut self,
        relative_path: &Path,
        placement: Placement,
        text: &str,
    ) -> WorkspaceResult<(DocumentId, Fingerprint)> {
        DocumentKind::from_path(relative_path)
            .ok_or_else(|| WorkspaceError::UnsupportedDocument(relative_path.to_path_buf()))?;
        self.ensure_placement_exists(&placement)?;

        if self.file.is_some() {
            return self.create_embedded_document(relative_path, placement, text);
        }
        let absolute = resolve_inside(&self.root, relative_path)?;
        let fingerprint = save_document(&absolute, text, SaveExpectation::New)?;
        let id = DocumentId::new();
        let entry = DocumentEntry {
            id,
            path: portable(relative_path),
            placement,
            associations: Vec::new(),
            execution: None,
        };

        let registered = self.update_manifest(|manifest| {
            manifest.documents.push(entry);
            Ok(id)
        });

        if registered.is_err() {
            // Keep disk and manifest consistent when the manifest cannot be saved.
            let _ = fs::remove_file(&absolute);
        }

        registered.map(|id| (id, fingerprint))
    }

    /// Registers an existing file in the workspace folder.
    pub fn register_document(
        &mut self,
        path: &Path,
        placement: Placement,
    ) -> WorkspaceResult<DocumentId> {
        if self.file.is_some() {
            let loaded = read_document(path)?;
            let name = path.file_name().ok_or(WorkspaceError::InvalidFile)?;
            return self
                .create_embedded_document(Path::new(name), placement, &loaded.text)
                .map(|(id, _)| id);
        }
        let relative = if path.is_absolute() {
            relative_to(&self.root, path)?
        } else {
            path.to_path_buf()
        };

        DocumentKind::from_path(&relative)
            .ok_or_else(|| WorkspaceError::UnsupportedDocument(relative.clone()))?;
        let absolute = resolve_inside(&self.root, &relative)?;

        if !absolute.is_file() {
            return Err(WorkspaceError::NotFound { kind: "file" });
        }

        let portable_path = portable(&relative);

        if let Some(existing) = self.manifest.document_by_path(&portable_path) {
            return Ok(existing.id);
        }

        self.ensure_placement_exists(&placement)?;
        let id = DocumentId::new();
        let entry = DocumentEntry {
            id,
            path: portable_path,
            placement,
            associations: Vec::new(),
            execution: None,
        };

        self.update_manifest(|manifest| {
            manifest.documents.push(entry);
            Ok(id)
        })
    }

    pub fn load_document(&self, id: DocumentId) -> WorkspaceResult<LoadedDocument> {
        self.manifest
            .document(id)
            .ok_or(WorkspaceError::NotFound { kind: "document" })?;
        if let Some(file) = &self.file {
            let text = file
                .contents
                .documents
                .get(&id.to_string())
                .ok_or(WorkspaceError::NotFound { kind: "document" })?
                .clone();
            return Ok(LoadedDocument {
                fingerprint: Fingerprint::of(text.as_bytes()),
                text,
                modified: None,
            });
        }
        read_document(&self.document_path(id)?)
    }

    pub fn save_document(
        &mut self,
        id: DocumentId,
        text: &str,
        expectation: SaveExpectation,
    ) -> WorkspaceResult<Fingerprint> {
        if self.file.is_some() {
            return self.save_embedded_document(id, text, expectation);
        }
        save_document(&self.document_path(id)?, text, expectation)
    }

    /// Renames the document's file within its folder.
    pub fn rename_document(&mut self, id: DocumentId, file_name: &str) -> WorkspaceResult<()> {
        validate_name(file_name)?;
        DocumentKind::from_path(Path::new(file_name))
            .ok_or_else(|| WorkspaceError::UnsupportedDocument(PathBuf::from(file_name)))?;

        if self.file.is_some() {
            return self.rename_embedded_document(id, file_name);
        }
        let current_relative = from_portable(
            &self
                .manifest
                .document(id)
                .ok_or(WorkspaceError::NotFound { kind: "document" })?
                .path,
        );
        let renamed_relative = current_relative.with_file_name(file_name);
        let from = resolve_inside(&self.root, &current_relative)?;
        let to = resolve_inside(&self.root, &renamed_relative)?;

        if to.symlink_metadata().is_ok() {
            return Err(WorkspaceError::AlreadyExists(renamed_relative));
        }

        fs::rename(&from, &to).map_err(|error| WorkspaceError::io(&from, error))?;
        let new_path = portable(&renamed_relative);

        let updated = self.update_manifest(|manifest| {
            if let Some(document) = manifest
                .documents
                .iter_mut()
                .find(|document| document.id == id)
            {
                document.path = new_path;
            }

            Ok(())
        });

        if updated.is_err() {
            let _ = fs::rename(&to, &from);
        }

        updated
    }

    /// Updates the manifest after a file was moved or renamed outside Qwerio.
    pub fn relink_document(
        &mut self,
        id: DocumentId,
        new_relative_path: &Path,
    ) -> WorkspaceResult<()> {
        let absolute = resolve_inside(&self.root, new_relative_path)?;

        if !absolute.is_file() {
            return Err(WorkspaceError::NotFound { kind: "file" });
        }

        let path = portable(new_relative_path);

        self.update_manifest(|manifest| {
            let document = manifest
                .documents
                .iter_mut()
                .find(|document| document.id == id)
                .ok_or(WorkspaceError::NotFound { kind: "document" })?;
            document.path = path;
            Ok(())
        })
    }

    /// Removes a document from the workspace, optionally deleting its file.
    pub fn remove_document(&mut self, id: DocumentId, delete_file: bool) -> WorkspaceResult<()> {
        let path = self.document_path(id)?;

        self.update_manifest(|manifest| {
            manifest.documents.retain(|document| document.id != id);
            Ok(())
        })?;

        if self.file.is_none() && delete_file && path.exists() {
            fs::remove_file(&path).map_err(|error| WorkspaceError::io(&path, error))?;
        }

        Ok(())
    }

    // --- Associations and bindings ------------------------------------------

    fn update_document(
        &mut self,
        id: DocumentId,
        change: impl FnOnce(&mut DocumentEntry),
    ) -> WorkspaceResult<()> {
        self.update_manifest(|manifest| {
            let document = manifest
                .documents
                .iter_mut()
                .find(|document| document.id == id)
                .ok_or(WorkspaceError::NotFound { kind: "document" })?;
            change(document);
            Ok(())
        })
    }

    pub fn associate(&mut self, id: DocumentId, object: ObjectRef) -> WorkspaceResult<()> {
        self.update_document(id, |document| {
            if !document
                .associations
                .iter()
                .any(|existing| existing.same_location_and_name(&object))
            {
                document.associations.push(object);
            }
        })
    }

    pub fn dissociate(&mut self, id: DocumentId, object: &ObjectRef) -> WorkspaceResult<()> {
        self.update_document(id, |document| {
            document
                .associations
                .retain(|existing| !existing.same_location_and_name(object));
        })
    }

    /// Repairs an association whose object was renamed or recreated.
    pub fn replace_association(
        &mut self,
        id: DocumentId,
        previous: &ObjectRef,
        replacement: ObjectRef,
    ) -> WorkspaceResult<()> {
        self.update_document(id, |document| {
            for association in &mut document.associations {
                if association.same_location_and_name(previous) {
                    *association = replacement.clone();
                }
            }
        })
    }

    pub fn set_execution_binding(
        &mut self,
        id: DocumentId,
        binding: Option<ExecutionBinding>,
    ) -> WorkspaceResult<()> {
        self.update_document(id, |document| document.execution = binding)
    }

    /// Associations whose object `exists` reports missing, for repair.
    pub fn unresolved_associations(
        &self,
        exists: impl Fn(&ObjectRef) -> bool,
    ) -> Vec<(DocumentId, ObjectRef)> {
        self.manifest
            .documents
            .iter()
            .flat_map(|document| {
                document
                    .associations
                    .iter()
                    .filter(|object| !exists(object))
                    .map(|object| (document.id, object.clone()))
            })
            .collect()
    }

    /// Supported document files in the folder that the manifest does not
    /// list, as portable relative paths. Hidden and build folders are skipped.
    pub fn untracked_documents(&self) -> WorkspaceResult<Vec<String>> {
        if self.file.is_some() {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        let mut pending = vec![(self.root.clone(), 0)];
        let mut scanned = 0;

        while let Some((directory, depth)) = pending.pop() {
            let entries =
                fs::read_dir(&directory).map_err(|error| WorkspaceError::io(&directory, error))?;

            for entry in entries.flatten() {
                scanned += 1;

                if scanned > MAX_SCANNED_ENTRIES {
                    return Ok(sorted(found));
                }

                let name = entry.file_name().to_string_lossy().into_owned();

                if name.starts_with('.') {
                    continue;
                }

                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();

                if file_type.is_dir() {
                    if depth < MAX_SCAN_DEPTH && !SKIPPED_DIRECTORIES.contains(&name.as_str()) {
                        pending.push((path, depth + 1));
                    }

                    continue;
                }

                if !file_type.is_file() || DocumentKind::from_path(&path).is_none() {
                    continue;
                }

                let relative = portable(path.strip_prefix(&self.root).unwrap_or(&path));

                if self.manifest.document_by_path(&relative).is_none() {
                    found.push(relative);
                }
            }
        }

        Ok(sorted(found))
    }
}

fn sorted(mut paths: Vec<String>) -> Vec<String> {
    paths.sort();
    paths
}

fn manifest_path(root: &Path) -> PathBuf {
    root.join(MANIFEST_DIRECTORY).join(MANIFEST_FILE)
}

fn canonical_directory(root: &Path) -> WorkspaceResult<PathBuf> {
    let canonical = root
        .canonicalize()
        .map_err(|error| WorkspaceError::io(root, error))?;

    if !canonical.is_dir() {
        return Err(WorkspaceError::io(
            root,
            std::io::Error::new(std::io::ErrorKind::NotADirectory, "not a folder"),
        ));
    }

    Ok(canonical)
}
