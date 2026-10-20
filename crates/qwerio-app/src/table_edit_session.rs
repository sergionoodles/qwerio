//! Table editing transitions independent of widgets and rendering.

use std::sync::Arc;

use qwerio_core::catalog::ObjectDetails;
use qwerio_core::cell_input::{CellEditor, initial_text, parse_cell_input};
use qwerio_core::edit::SavedRow;
use qwerio_core::error::DbResult;
use qwerio_core::paging::Page;
use qwerio_core::value::Value;

use crate::requests::Ticket;
use crate::table::TableBrowserState;
use crate::table_edits::{EditBatch, PendingEdits};

pub struct CellDraft {
    pub kind: CellEditor,
    pub is_nullable: bool,
    pub initial_text: String,
    is_initial_value_absent: bool,
    has_been_edited: bool,
}

impl CellDraft {
    pub fn new(kind: CellEditor, is_nullable: bool, value: Option<&Value>) -> Self {
        Self {
            kind,
            is_nullable,
            initial_text: value.map(initial_text).unwrap_or_default(),
            is_initial_value_absent: value.is_none_or(Value::is_null),
            has_been_edited: false,
        }
    }

    pub fn record_input_change(&mut self, text: &str) {
        if text != self.initial_text {
            self.has_been_edited = true;
        }
    }

    pub fn has_changes(&self, text: &str) -> bool {
        // NULL and DEFAULT share a blank editor, but clearing edited text
        // explicitly chooses an empty string instead of either original state.
        let has_explicit_empty_text = self.kind == CellEditor::Text
            && self.is_initial_value_absent
            && self.has_been_edited
            && text.is_empty();
        text != self.initial_text || has_explicit_empty_text
    }

    pub fn parse_change(&self, text: &str) -> Result<Option<Value>, String> {
        if !self.has_changes(text) {
            return Ok(None);
        }
        parse_cell_input(self.kind, text).map(Some)
    }
}

#[derive(Default)]
pub struct TableEditSession {
    pending: PendingEdits,
    is_saving: bool,
    deferred_pages: Vec<(Ticket, DbResult<Page>)>,
}

impl TableEditSession {
    pub fn pending(&self) -> &PendingEdits {
        &self.pending
    }

    pub fn is_saving(&self) -> bool {
        self.is_saving
    }

    pub fn defer_page(&mut self, ticket: Ticket, result: DbResult<Page>) {
        self.deferred_pages.push((ticket, result));
    }

    pub fn apply_deferred_pages(&mut self, browser: &mut TableBrowserState) {
        if self.is_saving {
            return;
        }
        for (ticket, result) in std::mem::take(&mut self.deferred_pages) {
            browser.apply_page(ticket, result);
        }
    }

    pub fn edit_pending<T>(&mut self, edit: impl FnOnce(&mut PendingEdits) -> T) -> Option<T> {
        if self.is_saving {
            return None;
        }
        Some(edit(&mut self.pending))
    }

    pub fn begin_save(&mut self, details: Arc<ObjectDetails>) -> Option<EditBatch> {
        if self.is_saving {
            return None;
        }
        let batch = self.pending.batch(details)?;
        self.is_saving = true;
        Some(batch)
    }

    pub fn finish_save(
        &mut self,
        browser: &mut TableBrowserState,
        batch: &EditBatch,
        rows: Option<Vec<SavedRow>>,
    ) {
        if let Some(rows) = rows {
            self.pending.apply_saved(browser, batch, rows);
        }
        self.is_saving = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_drafts_parse_changes_and_preserve_invalid_text_for_correction() {
        let draft = CellDraft::new(CellEditor::Integer, true, Some(&Value::Integer(4)));
        assert_eq!(draft.parse_change("4"), Ok(None));
        assert_eq!(draft.parse_change("12"), Ok(Some(Value::Integer(12))));
        assert!(draft.parse_change("invalid").is_err());
        assert!(draft.has_changes("invalid"));
    }

    #[test]
    fn clearing_null_or_default_text_is_an_explicit_empty_string() {
        for initial in [None, Some(Value::Null)] {
            let mut draft = CellDraft::new(CellEditor::Text, true, initial.as_ref());
            draft.record_input_change("");
            assert_eq!(draft.parse_change(""), Ok(None));

            draft.record_input_change("temporary");
            draft.record_input_change("");
            assert!(draft.has_changes(""));
            assert_eq!(draft.parse_change(""), Ok(Some(Value::Text(String::new()))));
        }
    }

    #[test]
    fn restoring_an_existing_text_value_leaves_no_change() {
        for original in ["", "original"] {
            let value = Value::Text(original.to_owned());
            let mut draft = CellDraft::new(CellEditor::Text, false, Some(&value));
            draft.record_input_change("temporary");
            draft.record_input_change(original);
            assert!(!draft.has_changes(original));
            assert_eq!(draft.parse_change(original), Ok(None));
        }
    }

    #[test]
    fn clearing_a_number_still_requires_valid_numeric_input() {
        let mut draft = CellDraft::new(CellEditor::Integer, true, Some(&Value::Integer(4)));
        draft.record_input_change("");
        assert!(draft.has_changes(""));
        assert!(draft.parse_change("").is_err());
    }

    fn browser() -> TableBrowserState {
        use qwerio_core::{
            catalog::{ColumnInfo, KeyInfo},
            ids::ConnectionId,
            paging::PagingMode,
            resource::{ObjectKind, ObjectRef},
            result::ColumnDescriptor,
        };
        let object = ObjectRef {
            connection: ConnectionId::new(),
            database: None,
            namespace: None,
            kind: ObjectKind::Table,
            name: "items".into(),
        };
        let columns = ["id", "value"]
            .into_iter()
            .enumerate()
            .map(|(ordinal, name)| ColumnInfo {
                name: name.into(),
                type_name: "integer".into(),
                cast_type: None,
                nullable: false,
                default: None,
                ordinal,
                comment: None,
                decodable: true,
                generated: false,
            })
            .collect();
        let details = Arc::new(ObjectDetails {
            object: object.clone(),
            columns,
            primary_key: Some(KeyInfo {
                name: None,
                columns: vec!["id".into()],
                allows_null: false,
            }),
            unique_keys: vec![],
            row_locator: None,
            foreign_keys: vec![],
            referencing_keys: vec![],
            comment: None,
        });
        let mut browser = TableBrowserState::new(object, crate::navigation::ViewState::default())
            .with_limits(1, 1);
        let fetch = browser.set_details(details);
        browser.apply_page(
            fetch.ticket,
            Ok(Page {
                columns: vec![
                    ColumnDescriptor::new("id", "integer"),
                    ColumnDescriptor::new("value", "integer"),
                ],
                rows: vec![vec![Value::Integer(1), Value::Integer(10)]],
                row_locators: vec![],
                next: None,
                mode: PagingMode::Keyset,
                notice: None,
            }),
        );
        browser
    }

    #[test]
    fn a_save_freezes_new_row_mutations_and_discard_until_it_finishes() {
        let mut browser = browser();
        let mut session = TableEditSession::default();
        session.edit_pending(|pending| pending.add_row(vec![("value".into(), Value::Integer(20))]));
        let batch = session
            .begin_save(browser.details().expect("details").clone())
            .expect("batch");
        assert!(
            session
                .edit_pending(|pending| pending.set_new_row_value(
                    0,
                    "value".into(),
                    Value::Integer(30)
                ))
                .is_none()
        );
        assert!(session.edit_pending(|pending| pending.clear()).is_none());
        session.finish_save(
            &mut browser,
            &batch,
            Some(vec![SavedRow {
                values: vec![Value::Integer(2), Value::Integer(20)],
                locator: None,
            }]),
        );
        assert!(!session.is_saving());
        assert!(session.pending().is_empty());
        assert_eq!(browser.rows()[0][1], Value::Integer(20));
    }

    #[test]
    fn a_failed_save_keeps_pending_changes_and_allows_correction() {
        let mut browser = browser();
        let mut session = TableEditSession::default();
        session.edit_pending(|pending| pending.stage(&browser, 0, 0, Value::Integer(2)));
        let batch = session
            .begin_save(browser.details().expect("details").clone())
            .expect("batch");
        assert!(
            session
                .edit_pending(|pending| pending.stage(&browser, 0, 1, Value::Integer(30)))
                .is_none()
        );
        session.finish_save(&mut browser, &batch, None);
        assert_eq!(session.pending().change_count(), 1);
        assert_eq!(
            session.edit_pending(|pending| pending.stage(&browser, 0, 1, Value::Integer(30))),
            Some(true)
        );
        assert_eq!(session.pending().change_count(), 2);
    }

    #[test]
    fn pages_deferred_during_a_save_are_applied_after_it_finishes() {
        let mut browser = browser();
        let mut session = TableEditSession::default();
        session.edit_pending(|pending| pending.stage(&browser, 0, 1, Value::Integer(11)));
        let batch = session
            .begin_save(browser.details().expect("details").clone())
            .expect("batch");
        let fetch = browser.refresh().expect("refresh");
        let page = qwerio_core::paging::PagePlan::new(&fetch.request)
            .expect("plan")
            .build_page(
                Vec::new(),
                vec![vec![
                    Value::Integer(1),
                    Value::Integer(99),
                    Value::Integer(1),
                ]],
            );
        session.defer_page(fetch.ticket, Ok(page));

        session.apply_deferred_pages(&mut browser);
        assert!(browser.rows().is_empty());

        session.finish_save(&mut browser, &batch, None);
        session.apply_deferred_pages(&mut browser);
        assert_eq!(
            browser.rows(),
            &[vec![Value::Integer(1), Value::Integer(99)]]
        );
    }
}
