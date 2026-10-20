//! File-workspace operations; legacy folders remain readable without rewriting their files.
use crate::document::{Fingerprint, SaveExpectation};
use crate::file::{Contents, StoredConnection, WorkspaceFile, document_key};
use crate::labels::{LabelAssignment, LabelTarget, validate_label};
use crate::manifest::{DocumentEntry, DocumentId, Manifest, Placement};
use crate::{Workspace, WorkspaceError, WorkspaceResult};
use qwerio_core::connection::Secret;
use std::path::Path;

impl Workspace {
    pub fn create_file(
        path: &Path,
        name: &str,
        password: Option<&Secret>,
    ) -> WorkspaceResult<Self> {
        let file = WorkspaceFile::create(path, Contents::new(Manifest::new(name)), password)?;
        Ok(Self::from_file(file))
    }

    pub fn open_file(path: &Path, password: Option<&Secret>) -> WorkspaceResult<Self> {
        WorkspaceFile::open(path, password).map(Self::from_file)
    }

    fn from_file(file: WorkspaceFile) -> Self {
        Self {
            root: file.path.clone(),
            manifest: file.contents.manifest.clone(),
            manifest_fingerprint: Fingerprint::of(&[]),
            file: Some(file),
        }
    }

    pub fn is_file_workspace(&self) -> bool {
        self.file.is_some()
    }
    pub fn is_encrypted(&self) -> bool {
        self.file.as_ref().is_some_and(WorkspaceFile::is_encrypted)
    }

    /// Exports a separate file with its own password; the open workspace is unchanged.
    pub fn export(&self, path: &Path, password: Option<&Secret>) -> WorkspaceResult<()> {
        self.export_with_connections(
            path,
            password,
            self.file
                .as_ref()
                .map(|file| file.contents.connections.clone())
                .unwrap_or_default(),
        )
    }

    pub fn export_with_connections(
        &self,
        path: &Path,
        password: Option<&Secret>,
        connections: Vec<StoredConnection>,
    ) -> WorkspaceResult<()> {
        self.export_with_state(path, password, connections, Vec::new(), None)
    }

    pub fn export_with_state(
        &self,
        path: &Path,
        password: Option<&Secret>,
        connections: Vec<StoredConnection>,
        drafts: Vec<(DocumentId, String)>,
        ui_state: Option<String>,
    ) -> WorkspaceResult<()> {
        let mut contents = match &self.file {
            Some(file) => file.contents.clone(),
            None => {
                let mut contents = Contents::new(self.manifest.clone());
                for document in &self.manifest.documents {
                    contents.documents.insert(
                        document_key(document.id),
                        self.load_document(document.id)?.text,
                    );
                }
                for path in self.untracked_documents()? {
                    let absolute = crate::paths::resolve_inside(
                        &self.root,
                        &crate::paths::from_portable(&path),
                    )?;
                    let loaded = crate::document::read_document(&absolute)?;
                    let id = DocumentId::new();
                    contents.manifest.documents.push(DocumentEntry {
                        id,
                        path,
                        placement: Placement::Root,
                        associations: Vec::new(),
                        execution: None,
                    });
                    contents.documents.insert(document_key(id), loaded.text);
                }
                contents
            }
        };
        contents.documents.retain(|id, _| {
            contents
                .manifest
                .documents
                .iter()
                .any(|document| document.id.to_string() == *id)
        });
        contents.connections = connections;
        if ui_state.is_some() {
            contents.ui_state = ui_state;
        }
        for (document, text) in drafts {
            contents.drafts.insert(document_key(document), text);
        }
        contents
            .drafts
            .retain(|id, _| contents.documents.contains_key(id));
        WorkspaceFile::create(path, contents, password).map(|_| ())
    }

    pub fn ui_state(&self) -> Option<&str> {
        self.file.as_ref()?.contents.ui_state.as_deref()
    }

    pub fn save_ui_state(&mut self, state: String) -> WorkspaceResult<()> {
        let file = self.file.as_mut().ok_or(WorkspaceError::InvalidFile)?;
        let mut contents = file.contents.clone();
        contents.ui_state = Some(state);
        file.save(contents)
    }

    pub fn stored_connection(
        &self,
        id: qwerio_core::ids::ConnectionId,
    ) -> Option<&StoredConnection> {
        self.file
            .as_ref()?
            .contents
            .connections
            .iter()
            .find(|connection| connection.id == id)
    }

    pub fn save_connection(&mut self, connection: StoredConnection) -> WorkspaceResult<()> {
        let file = self.file.as_mut().ok_or(WorkspaceError::InvalidFile)?;
        let mut contents = file.contents.clone();
        contents
            .connections
            .retain(|stored| stored.id != connection.id);
        contents.connections.push(connection);
        file.save(contents)
    }

    pub fn save_draft(
        &mut self,
        document: DocumentId,
        text: Option<String>,
    ) -> WorkspaceResult<()> {
        if text.is_some() && self.manifest.document(document).is_none() {
            return Err(WorkspaceError::NotFound { kind: "document" });
        }
        let file = self.file.as_mut().ok_or(WorkspaceError::InvalidFile)?;
        let mut contents = file.contents.clone();
        match text {
            Some(text) => {
                contents.drafts.insert(document_key(document), text);
            }
            None => {
                contents.drafts.remove(&document_key(document));
            }
        }
        file.save(contents)
    }

    pub fn draft(&self, document: DocumentId) -> Option<&str> {
        self.file
            .as_ref()?
            .contents
            .drafts
            .get(&document_key(document))
            .map(String::as_str)
    }

    pub(crate) fn create_embedded_document(
        &mut self,
        path: &Path,
        placement: Placement,
        text: &str,
    ) -> WorkspaceResult<(DocumentId, Fingerprint)> {
        if !crate::paths::is_plain_relative(path) {
            return Err(WorkspaceError::OutsideWorkspace(path.to_path_buf()));
        }
        let portable = crate::paths::portable(path);
        if self.manifest.document_by_path(&portable).is_some() {
            return Err(WorkspaceError::AlreadyExists(path.to_path_buf()));
        }
        let id = DocumentId::new();
        let file = self.file.as_mut().ok_or(WorkspaceError::InvalidFile)?;
        let mut contents = file.contents.clone();
        contents.manifest.documents.push(DocumentEntry {
            id,
            path: portable,
            placement,
            associations: Vec::new(),
            execution: None,
        });
        contents.documents.insert(document_key(id), text.to_owned());
        file.save(contents)?;
        self.manifest = file.contents.manifest.clone();
        Ok((id, Fingerprint::of(text.as_bytes())))
    }

    pub(crate) fn save_embedded_document(
        &mut self,
        id: DocumentId,
        text: &str,
        expectation: SaveExpectation,
    ) -> WorkspaceResult<Fingerprint> {
        let current = self.load_document(id)?.fingerprint;
        if matches!(expectation, SaveExpectation::New)
            || matches!(expectation, SaveExpectation::Unchanged(base) if base != current)
        {
            return Err(WorkspaceError::Conflict {
                path: self.root.clone(),
                on_disk: Some(current),
            });
        }
        let file = self.file.as_mut().ok_or(WorkspaceError::InvalidFile)?;
        let mut contents = file.contents.clone();
        contents.documents.insert(document_key(id), text.to_owned());
        contents.drafts.remove(&document_key(id));
        file.save(contents)?;
        Ok(Fingerprint::of(text.as_bytes()))
    }

    pub(crate) fn rename_embedded_document(
        &mut self,
        id: DocumentId,
        name: &str,
    ) -> WorkspaceResult<()> {
        let entry = self
            .manifest
            .document(id)
            .ok_or(WorkspaceError::NotFound { kind: "document" })?;
        let path = crate::paths::portable(&Path::new(&entry.path).with_file_name(name));
        if self.manifest.document_by_path(&path).is_some() {
            return Err(WorkspaceError::AlreadyExists(path.into()));
        }
        self.update_manifest(|manifest| {
            let entry = manifest
                .documents
                .iter_mut()
                .find(|entry| entry.id == id)
                .ok_or(WorkspaceError::NotFound { kind: "document" })?;
            entry.path = path;
            Ok(())
        })
    }

    pub fn define_label(&mut self, name: &str) -> WorkspaceResult<()> {
        let name = validate_label(name)?;
        self.update_manifest(|manifest| {
            if !manifest.labels.contains(&name) {
                manifest.labels.push(name);
            }
            Ok(())
        })
    }

    pub fn assign_labels(
        &mut self,
        target: LabelTarget,
        labels: Vec<String>,
    ) -> WorkspaceResult<()> {
        if labels
            .iter()
            .any(|label| !self.manifest.labels.contains(label))
        {
            return Err(WorkspaceError::NotFound {
                kind: "label definition",
            });
        }
        self.update_manifest(|manifest| {
            manifest
                .label_assignments
                .retain(|assignment| assignment.target != target);
            if !labels.is_empty() {
                manifest
                    .label_assignments
                    .push(LabelAssignment { target, labels });
            }
            Ok(())
        })
    }
}
