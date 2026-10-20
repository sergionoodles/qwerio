use std::time::SystemTime;

use qwerio_workspace::document::{Fingerprint, LoadedDocument, SaveExpectation};
use qwerio_workspace::manifest::DocumentId;

use super::WorkspaceStore;
use crate::local_state::Draft;
use crate::{AppError, AppResult};

pub struct SavedDocument {
    pub fingerprint: Fingerprint,
    /// The document was saved, but its obsolete local draft could not be removed.
    pub draft_error: Option<AppError>,
}

impl WorkspaceStore {
    pub fn load_document(
        &self,
        document: DocumentId,
        restore_draft: bool,
    ) -> AppResult<(LoadedDocument, Option<Draft>)> {
        let loaded = self.workspace.load_document(document)?;
        let draft = if restore_draft {
            self.draft(document, loaded.fingerprint)?
        } else {
            None
        };
        Ok((loaded, draft))
    }

    fn draft(&self, document: DocumentId, base: Fingerprint) -> AppResult<Option<Draft>> {
        if self.workspace.is_file_workspace() {
            return Ok(self.workspace.draft(document).map(|text| Draft {
                document,
                text: text.into(),
                base: Some(base),
            }));
        }
        Ok(self
            .services
            .local_state
            .drafts(self.workspace.manifest().id)?
            .into_iter()
            .find(|draft| draft.document == document))
    }

    pub fn save_draft(&mut self, draft: Draft) -> AppResult<()> {
        if self.workspace.is_file_workspace() {
            self.workspace
                .save_draft(draft.document, Some(draft.text))?;
            return Ok(());
        }
        self.services.local_state.save_draft(
            self.workspace.manifest().id,
            &draft,
            SystemTime::now(),
        )
    }

    pub fn discard_draft(&mut self, document: DocumentId) -> AppResult<()> {
        if self.workspace.is_file_workspace() {
            if self.workspace.draft(document).is_none() {
                return Ok(());
            }
            self.workspace.save_draft(document, None)?;
            return Ok(());
        }
        self.services
            .local_state
            .discard_draft(self.workspace.manifest().id, document)
    }

    pub fn save_document(
        &mut self,
        document: DocumentId,
        text: &str,
        expectation: SaveExpectation,
    ) -> AppResult<SavedDocument> {
        let fingerprint = self.workspace.save_document(document, text, expectation)?;
        let draft_error = if self.workspace.is_file_workspace() {
            None
        } else {
            self.discard_draft(document).err()
        };
        Ok(SavedDocument {
            fingerprint,
            draft_error,
        })
    }
}
