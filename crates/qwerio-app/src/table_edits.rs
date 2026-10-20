//! Row changes staged on a browsed table until they are saved together in
//! one transaction or discarded: edited cells, new rows, and deleted rows.
//! Changes to loaded rows are keyed by row identity and column name, so they
//! survive reloads, sorting, and rows being released from memory.

use std::sync::Arc;

use qwerio_core::catalog::ObjectDetails;
use qwerio_core::cell_input::cell_editor;
use qwerio_core::edit::{
    RowChange, RowEditRequest, RowIdentity, RowUpdate, SavedRow, is_copied_to_new_rows,
};
use qwerio_core::error::DbResult;
use qwerio_core::session::AccessMode;
use qwerio_core::sql::ColumnRef;
use qwerio_core::value::Value;

use crate::services::ResolvedConnection;
use crate::sessions::SessionRegistry;
use crate::table::TableBrowserState;

#[derive(Clone, Debug, Default)]
pub struct PendingEdits {
    updates: Vec<PendingUpdate>,
    /// Newest first, the order they are shown in above the loaded rows.
    new_rows: Vec<NewRow>,
    deletes: Vec<RowIdentity>,
    next_new_row_id: u64,
}

#[derive(Clone, Debug)]
struct PendingUpdate {
    identity: RowIdentity,
    changes: Vec<(String, Value)>,
}

#[derive(Clone, Debug)]
struct NewRow {
    id: NewRowId,
    /// Columns set so far; the others get their default.
    values: Vec<(String, Value)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NewRowId(u64);

/// A row of the grid: new rows come first, then the loaded rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridRow {
    /// Index into the new rows, newest first.
    New(usize),
    /// Index into the browser's loaded rows.
    Loaded(usize),
}

/// The request saving every staged change, and the new rows it inserts.
#[derive(Clone, Debug)]
pub struct EditBatch {
    pub request: RowEditRequest,
    /// The inserted new rows, in the order of the insert changes.
    inserted: Vec<NewRowId>,
}

impl PendingEdits {
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.new_rows.is_empty() && self.deletes.is_empty()
    }

    /// The number of unsaved changes: edited cells of rows that are kept,
    /// new rows, and deleted rows.
    pub fn change_count(&self) -> usize {
        let edited_cells: usize = self
            .updates
            .iter()
            .filter(|update| !self.deletes.contains(&update.identity))
            .map(|update| update.changes.len())
            .sum();

        edited_cells + self.new_rows.len() + self.deletes.len()
    }

    pub fn new_row_count(&self) -> usize {
        self.new_rows.len()
    }

    /// What a grid row shows.
    pub fn grid_row(&self, grid_index: usize) -> GridRow {
        match grid_index.checked_sub(self.new_rows.len()) {
            Some(loaded) => GridRow::Loaded(loaded),
            None => GridRow::New(grid_index),
        }
    }

    /// Stages a new value for a loaded cell. Staging the loaded value again
    /// drops the edit. Returns `false` when the row cannot be identified.
    pub fn stage(
        &mut self,
        browser: &TableBrowserState,
        row: usize,
        column: usize,
        value: Value,
    ) -> bool {
        let (Some(identity), Some(name), Some(original)) = (
            browser.row_identity(row),
            browser
                .columns()
                .get(column)
                .map(|column| column.name.clone()),
            browser.rows().get(row).and_then(|cells| cells.get(column)),
        ) else {
            return false;
        };

        if *original == value {
            self.remove(&identity, &name);
            return true;
        }

        let pending = match self
            .updates
            .iter()
            .position(|update| update.identity == identity)
        {
            Some(index) => &mut self.updates[index],
            None => {
                self.updates.push(PendingUpdate {
                    identity,
                    changes: Vec::new(),
                });
                self.updates.last_mut().expect("an update was just pushed")
            }
        };

        set_value(&mut pending.changes, name, value);
        true
    }

    /// Drops the staged value of one loaded cell.
    pub fn revert(&mut self, browser: &TableBrowserState, row: usize, column: usize) {
        let (Some(identity), Some(descriptor)) =
            (browser.row_identity(row), browser.columns().get(column))
        else {
            return;
        };

        self.remove(&identity, &descriptor.name);
    }

    fn remove(&mut self, identity: &[(ColumnRef, Value)], name: &str) {
        for update in self
            .updates
            .iter_mut()
            .filter(|update| update.identity == identity)
        {
            update.changes.retain(|(changed, _)| changed != name);
        }

        self.updates.retain(|update| !update.changes.is_empty());
    }

    /// The staged value of a loaded cell, if it was edited.
    pub fn value(&self, browser: &TableBrowserState, row: usize, column: usize) -> Option<&Value> {
        if self.updates.is_empty() {
            return None;
        }

        let identity = browser.row_identity(row)?;
        let name = &browser.columns().get(column)?.name;

        self.updates
            .iter()
            .find(|update| update.identity == identity)?
            .changes
            .iter()
            .find(|(changed, _)| changed == name)
            .map(|(_, value)| value)
    }

    /// Adds a new row above the others with these column values and returns
    /// its index.
    pub fn add_row(&mut self, values: Vec<(String, Value)>) -> usize {
        let id = NewRowId(self.next_new_row_id);
        self.next_new_row_id += 1;
        self.new_rows.insert(0, NewRow { id, values });
        0
    }

    /// The values a copy of a grid row starts with: what the row shows, minus
    /// key and generated columns, values that cannot be edited, and columns
    /// that are not set yet.
    pub fn copy_values(&self, browser: &TableBrowserState, row: GridRow) -> Vec<(String, Value)> {
        let Some(details) = browser.details() else {
            return Vec::new();
        };

        details
            .columns
            .iter()
            .filter(|column| is_copied_to_new_rows(details, column))
            .filter(|column| cell_editor(column).is_some())
            .filter_map(|column| {
                let value = match row {
                    GridRow::New(index) => self.new_row_value(index, &column.name)?,
                    GridRow::Loaded(index) => {
                        let position = browser.column_index(&column.name)?;
                        self.value(browser, index, position)
                            .or_else(|| browser.rows().get(index)?.get(position))?
                    }
                };

                let is_copyable = !matches!(value, Value::Unsupported(_));
                is_copyable.then(|| (column.name.clone(), value.clone()))
            })
            .collect()
    }

    /// The value set for a column of a new row; `None` means its default.
    pub fn new_row_value(&self, index: usize, name: &str) -> Option<&Value> {
        self.new_rows
            .get(index)?
            .values
            .iter()
            .find(|(column, _)| column == name)
            .map(|(_, value)| value)
    }

    pub fn set_new_row_value(&mut self, index: usize, name: String, value: Value) {
        if let Some(row) = self.new_rows.get_mut(index) {
            set_value(&mut row.values, name, value);
        }
    }

    /// Leaves a column of a new row to its default.
    pub fn reset_new_row_value(&mut self, index: usize, name: &str) {
        if let Some(row) = self.new_rows.get_mut(index) {
            row.values.retain(|(column, _)| column != name);
        }
    }

    pub fn remove_new_row(&mut self, index: usize) {
        if index < self.new_rows.len() {
            self.new_rows.remove(index);
        }
    }

    pub fn is_deleted(&self, browser: &TableBrowserState, row: usize) -> bool {
        !self.deletes.is_empty()
            && browser
                .row_identity(row)
                .is_some_and(|identity| self.deletes.contains(&identity))
    }

    /// Marks a loaded row for deletion, or keeps it again. Its cell edits are
    /// kept but not saved while it is marked. Returns `false` when the row
    /// cannot be identified.
    pub fn toggle_deleted(&mut self, browser: &TableBrowserState, row: usize) -> bool {
        let Some(identity) = browser.row_identity(row) else {
            return false;
        };

        match self.deletes.iter().position(|deleted| *deleted == identity) {
            Some(index) => {
                self.deletes.remove(index);
            }
            None => self.deletes.push(identity),
        }

        true
    }

    pub fn clear(&mut self) {
        self.updates.clear();
        self.new_rows.clear();
        self.deletes.clear();
    }

    /// Drops values of columns the table no longer has, after its metadata
    /// was reloaded.
    pub fn retain_columns(&mut self, details: &ObjectDetails) {
        let exists = |(name, _): &(String, Value)| details.column(name).is_some();

        for update in &mut self.updates {
            update.changes.retain(exists);
        }

        for row in &mut self.new_rows {
            row.values.retain(exists);
        }

        self.updates.retain(|update| !update.changes.is_empty());
    }

    /// Every staged change, or `None` when nothing is staged. Deletes run
    /// first so their keys can be reused, then updates of kept rows, then
    /// inserts in the order the rows were added.
    pub fn batch(&self, details: Arc<ObjectDetails>) -> Option<EditBatch> {
        if self.is_empty() {
            return None;
        }

        let deletes = self.deletes.iter().map(|identity| RowChange::Delete {
            identity: identity.clone(),
        });
        let updates = self
            .updates
            .iter()
            .filter(|update| !self.deletes.contains(&update.identity))
            .map(|update| {
                RowChange::Update(RowUpdate {
                    identity: update.identity.clone(),
                    changes: update.changes.clone(),
                })
            });
        let inserts = self.new_rows.iter().rev().map(|row| RowChange::Insert {
            values: row.values.clone(),
        });

        Some(EditBatch {
            request: RowEditRequest {
                details,
                changes: deletes.chain(updates).chain(inserts).collect(),
            },
            inserted: self.new_rows.iter().rev().map(|row| row.id).collect(),
        })
    }

    /// Shows the saved rows in the browser and drops the changes they saved.
    /// Cell edits staged while the save ran stay pending.
    pub fn apply_saved(
        &mut self,
        browser: &mut TableBrowserState,
        batch: &EditBatch,
        rows: Vec<SavedRow>,
    ) {
        let mut inserted = Vec::new();

        for (change, row) in batch.request.changes.iter().zip(rows) {
            match change {
                RowChange::Delete { identity } => {
                    browser.remove_row(identity);
                    self.deletes.retain(|deleted| deleted != identity);
                    self.updates.retain(|update| update.identity != *identity);
                }
                RowChange::Update(update) => {
                    browser.replace_row(&update.identity, row.values);

                    for (name, saved) in &update.changes {
                        self.remove_if_unchanged(&update.identity, name, saved);
                    }
                }
                RowChange::Insert { .. } => inserted.push(row),
            }
        }

        // Saved new rows keep their place above the loaded rows.
        inserted.reverse();
        browser.prepend_rows(inserted);
        self.new_rows
            .retain(|row| !batch.inserted.contains(&row.id));
    }

    fn remove_if_unchanged(&mut self, identity: &[(ColumnRef, Value)], name: &str, saved: &Value) {
        let is_unchanged = self
            .updates
            .iter()
            .filter(|update| update.identity == identity)
            .flat_map(|update| &update.changes)
            .any(|(changed, value)| changed == name && value == saved);

        if is_unchanged {
            self.remove(identity, name);
        }
    }
}

/// Sets the value of a column in a list of column values.
fn set_value(values: &mut Vec<(String, Value)>, name: String, value: Value) {
    match values.iter_mut().find(|(column, _)| *column == name) {
        Some((_, current)) => *current = value,
        None => values.push((name, value)),
    }
}

/// Saves row changes in one transaction on a writable session of their own,
/// which is closed afterwards. Returns the touched rows in request order.
pub async fn save_row_edits(
    sessions: &Arc<SessionRegistry>,
    connection: ResolvedConnection,
    request: RowEditRequest,
) -> DbResult<Vec<SavedRow>> {
    let database = request.details.object.database.clone();
    let lease = sessions
        .open_console(
            connection.descriptor,
            connection.credentials,
            database,
            AccessMode::ReadWrite,
        )
        .await?;

    let saved = lease.session().apply_row_edits(request).await;
    let closed = lease.session().close().await;
    let rows = saved?;
    closed?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use qwerio_core::catalog::{ColumnInfo, KeyInfo};
    use qwerio_core::ids::ConnectionId;
    use qwerio_core::paging::PagePlan;
    use qwerio_core::resource::{ObjectKind, ObjectRef};
    use qwerio_core::result::ColumnDescriptor;

    use super::*;
    use crate::navigation::ViewState;

    fn details() -> Arc<ObjectDetails> {
        let column = |name: &str, ordinal| ColumnInfo {
            name: name.to_owned(),
            type_name: "integer".to_owned(),
            cast_type: None,
            nullable: true,
            default: None,
            ordinal,
            comment: None,
            decodable: true,
            generated: false,
        };

        Arc::new(ObjectDetails {
            object: ObjectRef {
                connection: ConnectionId::new(),
                database: None,
                namespace: None,
                kind: ObjectKind::Table,
                name: "items".to_owned(),
            },
            columns: vec![column("id", 1), column("value", 2)],
            primary_key: Some(KeyInfo {
                name: None,
                columns: vec!["id".to_owned()],
                allows_null: false,
            }),
            unique_keys: Vec::new(),
            row_locator: None,
            foreign_keys: Vec::new(),
            referencing_keys: Vec::new(),
            comment: None,
        })
    }

    /// A browser with rows `(1, 10)`, `(2, 20)`, `(3, 30)` loaded.
    fn browser() -> TableBrowserState {
        let details = details();
        let mut browser = TableBrowserState::new(details.object.clone(), ViewState::default());
        let fetch = browser.set_details(details);
        let plan = PagePlan::new(&fetch.request).expect("plan");
        let rows = (1..=3)
            .map(|id| {
                vec![
                    Value::Integer(id),
                    Value::Integer(id * 10),
                    Value::Integer(id),
                ]
            })
            .collect();
        let page = plan.build_page(
            vec![
                ColumnDescriptor::new("id", "integer"),
                ColumnDescriptor::new("value", "integer"),
            ],
            rows,
        );
        browser.apply_page(fetch.ticket, Ok(page));
        browser
    }

    fn id_key(id: i64) -> RowIdentity {
        vec![(ColumnRef::Column("id".to_owned()), Value::Integer(id))]
    }

    fn value(value: i64) -> (String, Value) {
        ("value".to_owned(), Value::Integer(value))
    }

    fn saved(id: i64, value: i64) -> SavedRow {
        SavedRow {
            values: vec![Value::Integer(id), Value::Integer(value)],
            locator: None,
        }
    }

    #[test]
    fn staged_cells_become_one_update_per_row() {
        let browser = browser();
        let mut edits = PendingEdits::default();

        edits.stage(&browser, 1, 1, Value::Integer(21));
        edits.stage(&browser, 1, 0, Value::Integer(9));
        edits.stage(&browser, 2, 1, Value::Null);
        edits.stage(&browser, 1, 1, Value::Integer(22));

        let batch = edits.batch(details()).expect("batch");

        assert_eq!(edits.change_count(), 3);
        assert_eq!(edits.value(&browser, 1, 1), Some(&Value::Integer(22)));
        assert_eq!(edits.value(&browser, 0, 1), None);
        assert_eq!(
            batch.request.changes,
            vec![
                RowChange::Update(RowUpdate {
                    identity: id_key(2),
                    changes: vec![value(22), ("id".to_owned(), Value::Integer(9))],
                }),
                RowChange::Update(RowUpdate {
                    identity: id_key(3),
                    changes: vec![("value".to_owned(), Value::Null)],
                }),
            ]
        );
    }

    #[test]
    fn staging_the_loaded_value_or_reverting_drops_the_edit() {
        let browser = browser();
        let mut edits = PendingEdits::default();

        edits.stage(&browser, 0, 1, Value::Integer(11));
        edits.stage(&browser, 0, 1, Value::Integer(10));
        edits.stage(&browser, 2, 1, Value::Integer(31));
        edits.revert(&browser, 2, 1);

        assert!(edits.is_empty());
        assert!(edits.batch(details()).is_none());
    }

    #[test]
    fn new_rows_come_first_in_the_grid_newest_on_top() {
        let mut edits = PendingEdits::default();
        edits.add_row(vec![value(1)]);
        edits.add_row(vec![value(2)]);

        assert_eq!(edits.grid_row(0), GridRow::New(0));
        assert_eq!(edits.grid_row(1), GridRow::New(1));
        assert_eq!(edits.grid_row(2), GridRow::Loaded(0));
        assert_eq!(edits.new_row_value(0, "value"), Some(&Value::Integer(2)));
    }

    #[test]
    fn batches_delete_then_update_kept_rows_then_insert_in_added_order() {
        let browser = browser();
        let mut edits = PendingEdits::default();
        edits.stage(&browser, 0, 1, Value::Integer(11));
        edits.stage(&browser, 1, 1, Value::Integer(21));
        edits.toggle_deleted(&browser, 1);
        edits.add_row(vec![value(1)]);
        edits.add_row(Vec::new());
        edits.set_new_row_value(0, "value".to_owned(), Value::Integer(2));

        let batch = edits.batch(details()).expect("batch");

        assert!(edits.is_deleted(&browser, 1));
        assert_eq!(edits.change_count(), 4);
        assert_eq!(
            batch.request.changes,
            vec![
                RowChange::Delete {
                    identity: id_key(2)
                },
                RowChange::Update(RowUpdate {
                    identity: id_key(1),
                    changes: vec![value(11)],
                }),
                RowChange::Insert {
                    values: vec![value(1)]
                },
                RowChange::Insert {
                    values: vec![value(2)]
                },
            ]
        );
    }

    #[test]
    fn deleting_twice_keeps_the_row_and_its_edits() {
        let browser = browser();
        let mut edits = PendingEdits::default();
        edits.stage(&browser, 1, 1, Value::Integer(21));

        edits.toggle_deleted(&browser, 1);
        edits.toggle_deleted(&browser, 1);

        assert!(!edits.is_deleted(&browser, 1));
        assert_eq!(edits.change_count(), 1);
    }

    #[test]
    fn copies_take_shown_values_without_key_columns() {
        let browser = browser();
        let mut edits = PendingEdits::default();
        edits.stage(&browser, 1, 1, Value::Integer(21));

        let copy = edits.copy_values(&browser, GridRow::Loaded(1));
        edits.add_row(copy.clone());
        edits.reset_new_row_value(0, "value");
        let copy_of_defaults = edits.copy_values(&browser, GridRow::New(0));

        assert_eq!(copy, vec![value(21)]);
        assert!(copy_of_defaults.is_empty());
    }

    #[test]
    fn saved_rows_replace_loaded_rows_and_clear_their_edits() {
        let mut browser = browser();
        let mut edits = PendingEdits::default();
        edits.stage(&browser, 0, 0, Value::Integer(100));
        edits.stage(&browser, 1, 1, Value::Integer(21));
        let batch = edits.batch(details()).expect("batch");

        // Edited again while the save ran: that edit must stay pending.
        edits.stage(&browser, 1, 1, Value::Integer(22));

        edits.apply_saved(&mut browser, &batch, vec![saved(100, 10), saved(2, 21)]);

        assert_eq!(
            browser.rows()[0],
            vec![Value::Integer(100), Value::Integer(10)]
        );
        assert_eq!(
            browser.rows()[1],
            vec![Value::Integer(2), Value::Integer(21)]
        );
        assert_eq!(edits.change_count(), 1);
        assert_eq!(edits.value(&browser, 1, 1), Some(&Value::Integer(22)));
    }

    #[test]
    fn saved_deletes_and_inserts_update_the_loaded_rows() {
        let mut browser = browser();
        let mut edits = PendingEdits::default();
        edits.toggle_deleted(&browser, 0);
        edits.add_row(vec![value(40)]);
        edits.add_row(vec![value(50)]);
        let batch = edits.batch(details()).expect("batch");

        // Added while the save ran: it stays a new row.
        edits.add_row(vec![value(60)]);

        edits.apply_saved(
            &mut browser,
            &batch,
            vec![saved(1, 10), saved(4, 40), saved(5, 50)],
        );

        let ids: Vec<&Value> = browser.rows().iter().map(|row| &row[0]).collect();
        assert_eq!(
            ids,
            vec![
                &Value::Integer(5),
                &Value::Integer(4),
                &Value::Integer(2),
                &Value::Integer(3)
            ]
        );
        assert_eq!(edits.new_row_count(), 1);
        assert_eq!(edits.new_row_value(0, "value"), Some(&Value::Integer(60)));
        assert_eq!(edits.change_count(), 1);
    }

    #[test]
    fn values_of_removed_columns_are_dropped() {
        let browser = browser();
        let mut edits = PendingEdits::default();
        edits.stage(&browser, 0, 1, Value::Integer(11));
        edits.add_row(vec![value(1)]);

        let mut without_value = (*details()).clone();
        without_value.columns.truncate(1);
        edits.retain_columns(&without_value);

        assert_eq!(edits.change_count(), 1);
        assert_eq!(edits.new_row_value(0, "value"), None);
    }
}
