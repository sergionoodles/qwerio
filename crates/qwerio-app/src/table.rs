//! State of one table or view being browsed: filters and sort applied by the
//! database, incremental pages, bounded row retention, and stale-result
//! rejection. Views drive it; it never touches a session itself.

use std::sync::Arc;

use qwerio_core::catalog::{ForeignKeyInfo, ObjectDetails};
use qwerio_core::edit::SavedRow;
use qwerio_core::error::{DatabaseError, DbResult};
use qwerio_core::filter::{Filter, SortDirection, SortKey};
use qwerio_core::paging::{ContinuationToken, Page, PageNotice, PageRequest, PagingMode};
use qwerio_core::relationship::{RelationshipOutcome, follow_referenced, follow_referencing};
use qwerio_core::resource::ObjectRef;
use qwerio_core::result::{ColumnDescriptor, Row};
use qwerio_core::sql::{ColumnRef, row_identity};
use qwerio_core::value::Value;

use crate::navigation::ViewState;
use crate::requests::{Ticket, TicketIssuer};

pub const DEFAULT_PAGE_ROWS: usize = 200;

/// Rows retained in memory. Older pages are released beyond this.
pub const DEFAULT_RETAINED_ROWS: usize = 5_000;

/// Largest value shown in the inspector; larger values are cut with a notice.
pub const MAX_INSPECTED_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum LoadStatus {
    /// Waiting for object metadata.
    NeedsDetails,
    Idle,
    Loading,
    LoadingMore,
    Failed(DatabaseError),
}

/// A page request tagged with the ticket that must accompany its result.
#[derive(Clone, Debug)]
pub struct PageFetch {
    pub ticket: Ticket,
    pub request: PageRequest,
}

#[derive(Debug)]
pub struct TableBrowserState {
    object: ObjectRef,
    details: Option<Arc<ObjectDetails>>,
    view: ViewState,
    columns: Vec<ColumnDescriptor>,
    rows: Vec<Row>,
    /// Each row's hidden locator, parallel to `rows`, when rows are
    /// identified by it; empty otherwise.
    row_locators: Vec<Value>,
    /// Rows released from the front of `rows` to stay within the budget.
    released_rows: usize,
    next: Option<ContinuationToken>,
    mode: Option<PagingMode>,
    notice: Option<PageNotice>,
    status: LoadStatus,
    tickets: TicketIssuer,
    page_rows: usize,
    retained_rows: usize,
}

impl TableBrowserState {
    pub fn new(object: ObjectRef, view: ViewState) -> Self {
        Self {
            object,
            details: None,
            view,
            columns: Vec::new(),
            rows: Vec::new(),
            row_locators: Vec::new(),
            released_rows: 0,
            next: None,
            mode: None,
            notice: None,
            status: LoadStatus::NeedsDetails,
            tickets: TicketIssuer::new(),
            page_rows: DEFAULT_PAGE_ROWS,
            retained_rows: DEFAULT_RETAINED_ROWS,
        }
    }

    pub fn with_limits(mut self, page_rows: usize, retained_rows: usize) -> Self {
        self.page_rows = page_rows.max(1);
        self.retained_rows = retained_rows.max(self.page_rows);
        self
    }

    pub fn object(&self) -> &ObjectRef {
        &self.object
    }

    pub fn details(&self) -> Option<&Arc<ObjectDetails>> {
        self.details.as_ref()
    }

    pub fn view(&self) -> &ViewState {
        &self.view
    }

    pub fn columns(&self) -> &[ColumnDescriptor] {
        &self.columns
    }

    /// Visible column indexes, honoring hidden columns.
    pub fn visible_columns(&self) -> Vec<usize> {
        self.columns
            .iter()
            .enumerate()
            .filter(|(_, column)| !self.view.hidden_columns.contains(&column.name))
            .map(|(index, _)| index)
            .collect()
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Rows released from the front because of the retention budget; the
    /// first retained row is row number `released_rows`.
    pub fn released_rows(&self) -> usize {
        self.released_rows
    }

    pub fn has_more(&self) -> bool {
        self.next.is_some()
    }

    pub fn paging_mode(&self) -> Option<PagingMode> {
        self.mode
    }

    pub fn notice(&self) -> Option<PageNotice> {
        self.notice
    }

    pub fn status(&self) -> &LoadStatus {
        &self.status
    }

    /// Applies metadata, which starts a fresh query.
    pub fn set_details(&mut self, details: Arc<ObjectDetails>) -> PageFetch {
        self.object = details.object.clone();
        self.view
            .filters
            .retain(|filter| details.column(&filter.column).is_some());
        self.view
            .sort
            .retain(|key| details.column(&key.column).is_some());
        self.details = Some(details);
        self.restart()
    }

    pub fn details_failed(&mut self, error: DatabaseError) {
        self.tickets.invalidate();
        self.status = LoadStatus::Failed(error);
    }

    /// Changes filters; the whole table is queried again.
    pub fn set_filters(&mut self, filters: Vec<Filter>) -> Option<PageFetch> {
        if self.view.filters == filters {
            return None;
        }

        self.view.filters = filters;
        self.restart_if_ready()
    }

    pub fn set_sort(&mut self, sort: Vec<SortKey>) -> Option<PageFetch> {
        if self.view.sort == sort {
            return None;
        }

        self.view.sort = sort;
        self.restart_if_ready()
    }

    /// Cycles a column through ascending, descending, and unsorted.
    pub fn toggle_sort(&mut self, column: &str) -> Option<PageFetch> {
        let current = self.view.sort.first().filter(|key| key.column == column);

        let sort = match current.map(|key| key.direction) {
            None => vec![SortKey::ascending(column)],
            Some(SortDirection::Ascending) => vec![SortKey::descending(column)],
            Some(SortDirection::Descending) => Vec::new(),
        };

        self.set_sort(sort)
    }

    pub fn set_column_hidden(&mut self, column: &str, hidden: bool) {
        self.view.hidden_columns.retain(|name| name != column);

        if hidden {
            self.view.hidden_columns.push(column.to_owned());
        }
    }

    /// Reloads from the first page with the same filters and sort.
    pub fn refresh(&mut self) -> Option<PageFetch> {
        self.restart_if_ready()
    }

    fn restart_if_ready(&mut self) -> Option<PageFetch> {
        self.details.is_some().then(|| self.restart())
    }

    fn restart(&mut self) -> PageFetch {
        self.tickets.invalidate();
        self.rows.clear();
        self.row_locators.clear();
        self.released_rows = 0;
        self.next = None;
        self.notice = None;
        self.status = LoadStatus::Loading;

        let ticket = self.tickets.issue();
        PageFetch {
            ticket,
            request: self.page_request(None),
        }
    }

    fn page_request(&self, continuation: Option<ContinuationToken>) -> PageRequest {
        let details = self
            .details
            .clone()
            .unwrap_or_else(|| Arc::new(empty_details(&self.object)));

        PageRequest {
            filters: self.view.filters.clone(),
            sort: self.view.sort.clone(),
            page_size: self.page_rows,
            continuation,
            ..PageRequest::first(details)
        }
    }

    /// Requests the next page when one exists and nothing is loading.
    pub fn load_more(&mut self) -> Option<PageFetch> {
        if self.status != LoadStatus::Idle {
            return None;
        }

        let continuation = self.next.clone()?;
        self.status = LoadStatus::LoadingMore;
        let ticket = self.tickets.issue();

        Some(PageFetch {
            ticket,
            request: self.page_request(Some(continuation)),
        })
    }

    /// Applies a page result. Results for superseded tickets are ignored and
    /// return `false`.
    pub fn apply_page(&mut self, ticket: Ticket, result: DbResult<Page>) -> bool {
        if !self.tickets.complete(ticket) {
            return false;
        }

        let page = match result {
            Ok(page) => page,
            Err(error) => {
                self.status = LoadStatus::Failed(error);
                return true;
            }
        };

        if self.columns.is_empty() || self.rows.is_empty() {
            self.columns = page.columns;
        }

        self.rows.extend(page.rows);
        self.row_locators.extend(page.row_locators);
        self.next = page.next;
        self.mode = Some(page.mode);
        self.notice = page.notice;
        self.status = LoadStatus::Idle;
        self.release_excess_rows();
        true
    }

    fn release_excess_rows(&mut self) {
        let excess = self.rows.len().saturating_sub(self.retained_rows);

        if excess > 0 {
            self.rows.drain(..excess);
            self.row_locators
                .drain(..excess.min(self.row_locators.len()));
            self.released_rows += excess;
        }
    }

    pub fn select_row(&mut self, index: Option<usize>) {
        self.view.selected_row_key = index.and_then(|index| self.row_key(index));
    }

    pub fn selected_row(&self) -> Option<usize> {
        let key = self.view.selected_row_key.as_ref()?;
        (0..self.rows.len()).find(|index| self.row_key(*index).as_ref() == Some(key))
    }

    /// The unique key values of a row, used to restore selection.
    fn row_key(&self, index: usize) -> Option<Vec<Value>> {
        let row = self.rows.get(index)?;
        let details = self.details.as_ref()?;

        match details.unique_row_key() {
            Some(key) => key
                .columns
                .iter()
                .map(|name| {
                    let position = details
                        .columns
                        .iter()
                        .position(|column| &column.name == name)?;
                    row.get(position).cloned()
                })
                .collect(),
            None => Some(row.clone()),
        }
    }

    /// The values that find a row in the database for updates: its unique
    /// key, or its hidden locator. `None` when rows have no identity.
    pub fn row_identity(&self, index: usize) -> Option<Vec<(ColumnRef, Value)>> {
        let row = self.rows.get(index)?;
        let details = self.details.as_ref()?;

        row_identity(details)?
            .into_iter()
            .map(|column| {
                let value = match &column {
                    ColumnRef::Column(name) => row.get(self.column_index(name)?)?.clone(),
                    ColumnRef::Locator => self.row_locators.get(index)?.clone(),
                };

                Some((column, value))
            })
            .collect()
    }

    /// The position of a column in rows.
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|column| column.name == name)
    }

    /// Replaces the loaded row with this identity by its saved values. The
    /// row stays put even when the change moves it out of the current sort or
    /// filters, until the next refresh.
    pub fn replace_row(&mut self, identity: &[(ColumnRef, Value)], row: Row) -> bool {
        let Some(index) = self.row_index(identity) else {
            return false;
        };

        self.rows[index] = row;
        true
    }

    /// Removes the loaded row with this identity, after it was deleted.
    pub fn remove_row(&mut self, identity: &[(ColumnRef, Value)]) -> bool {
        let Some(index) = self.row_index(identity) else {
            return false;
        };

        self.rows.remove(index);

        if index < self.row_locators.len() {
            self.row_locators.remove(index);
        }

        true
    }

    /// Shows newly inserted rows first, in the given order, until the next
    /// refresh puts them where the sort and filters place them.
    pub fn prepend_rows(&mut self, rows: Vec<SavedRow>) {
        let (values, locators): (Vec<Row>, Vec<Option<Value>>) = rows
            .into_iter()
            .map(|row| (row.values, row.locator))
            .unzip();

        self.rows.splice(0..0, values);
        self.row_locators
            .splice(0..0, locators.into_iter().flatten());
    }

    fn row_index(&self, identity: &[(ColumnRef, Value)]) -> Option<usize> {
        (0..self.rows.len()).find(|index| self.row_identity(*index).as_deref() == Some(identity))
    }

    /// Follows a declared foreign key from a row to the referenced row.
    pub fn follow_reference(
        &self,
        row: usize,
        foreign_key: &ForeignKeyInfo,
    ) -> Option<RelationshipOutcome> {
        Some(follow_referenced(
            self.details.as_ref()?,
            self.rows.get(row)?,
            foreign_key,
        ))
    }

    /// Follows a foreign key on another table that points at this row.
    pub fn follow_referencing(
        &self,
        row: usize,
        foreign_key: &ForeignKeyInfo,
    ) -> Option<RelationshipOutcome> {
        Some(follow_referencing(
            self.details.as_ref()?,
            self.rows.get(row)?,
            foreign_key,
        ))
    }

    /// The full text of one cell for the inspector, cut at
    /// [`MAX_INSPECTED_BYTES`] with an explicit flag.
    pub fn inspect_cell(&self, row: usize, column: usize) -> Option<CellInspection> {
        let value = self.rows.get(row)?.get(column)?;
        let text = value.canonical_text();
        let total_bytes = text.as_ref().map_or(0, String::len);
        let shown = text.map(|text| truncate_to_boundary(text, MAX_INSPECTED_BYTES));

        Some(CellInspection {
            column: self.columns.get(column)?.clone(),
            storage: value.storage_name(),
            is_null: value.is_null(),
            is_truncated: shown
                .as_ref()
                .is_some_and(|shown| shown.len() < total_bytes),
            text: shown,
            total_bytes,
        })
    }
}

fn truncate_to_boundary(mut text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }

    let mut end = max_bytes;

    while !text.is_char_boundary(end) {
        end -= 1;
    }

    text.truncate(end);
    text
}

fn empty_details(object: &ObjectRef) -> ObjectDetails {
    ObjectDetails {
        object: object.clone(),
        columns: Vec::new(),
        primary_key: None,
        unique_keys: Vec::new(),
        row_locator: None,
        foreign_keys: Vec::new(),
        referencing_keys: Vec::new(),
        comment: None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CellInspection {
    pub column: ColumnDescriptor,
    pub storage: &'static str,
    pub is_null: bool,
    /// `None` for NULL and for values without a text form.
    pub text: Option<String>,
    pub total_bytes: usize,
    pub is_truncated: bool,
}

#[cfg(test)]
mod tests {
    use qwerio_core::catalog::{ColumnInfo, KeyInfo};
    use qwerio_core::error::ErrorKind;
    use qwerio_core::ids::ConnectionId;
    use qwerio_core::resource::ObjectKind;

    use super::*;

    fn details() -> Arc<ObjectDetails> {
        let object = ObjectRef {
            connection: ConnectionId::new(),
            database: None,
            namespace: None,
            kind: ObjectKind::Table,
            name: "items".to_owned(),
        };
        let column = |name: &str, ordinal| ColumnInfo {
            name: name.to_owned(),
            type_name: "integer".to_owned(),
            cast_type: None,
            nullable: false,
            default: None,
            ordinal,
            comment: None,
            decodable: true,
            generated: false,
        };

        Arc::new(ObjectDetails {
            columns: vec![column("id", 1), column("value", 2)],
            primary_key: Some(KeyInfo {
                name: None,
                columns: vec!["id".to_owned()],
                allows_null: false,
            }),
            ..empty_details(&object)
        })
    }

    fn page(ids: std::ops::Range<i64>, has_more: bool, previous: &PageFetch) -> Page {
        let plan = qwerio_core::paging::PagePlan::new(&previous.request).expect("plan");
        let mut rows: Vec<Row> = ids
            .map(|id| vec![Value::Integer(id), Value::Integer(id * 10)])
            .collect();

        if has_more {
            let last = rows.last().cloned().unwrap_or_default();
            rows.push(last);
        }

        let with_keys = rows
            .into_iter()
            .map(|mut row| {
                let id = row[0].clone();
                row.push(id);
                row
            })
            .collect();

        plan.build_page(
            vec![
                ColumnDescriptor::new("id", "integer"),
                ColumnDescriptor::new("value", "integer"),
            ],
            with_keys,
        )
    }

    fn state() -> (TableBrowserState, PageFetch) {
        let details = details();
        let mut state =
            TableBrowserState::new(details.object.clone(), ViewState::default()).with_limits(3, 6);
        let fetch = state.set_details(details);
        (state, fetch)
    }

    #[test]
    fn stale_pages_are_ignored_after_filters_change() {
        let (mut state, first) = state();
        let refetch = state
            .set_filters(vec![Filter::is_null("value")])
            .expect("filters restart the query");

        assert!(!state.apply_page(first.ticket, Ok(page(0..3, true, &first))));
        assert!(state.rows().is_empty());

        assert!(state.apply_page(refetch.ticket, Ok(page(0..1, false, &refetch))));
        assert_eq!(state.rows().len(), 1);
        assert!(!state.has_more());
    }

    #[test]
    fn pages_append_and_older_rows_are_released_beyond_the_budget() {
        let (mut state, first) = state();
        state.apply_page(first.ticket, Ok(page(0..3, true, &first)));

        for start in [3, 6] {
            let more = state.load_more().expect("next page");
            assert!(state.apply_page(more.ticket, Ok(page(start..start + 3, true, &more))));
        }

        assert_eq!(state.rows().len(), 6);
        assert_eq!(state.released_rows(), 3);
        assert_eq!(state.rows()[0][0], Value::Integer(3));
    }

    #[test]
    fn row_identities_use_locators_that_stay_aligned_after_release() {
        let keyless = Arc::new(ObjectDetails {
            primary_key: None,
            row_locator: Some(qwerio_core::catalog::RowLocator {
                expression: "rowid".to_owned(),
                type_name: "integer".to_owned(),
            }),
            ..(*details()).clone()
        });
        let mut state =
            TableBrowserState::new(keyless.object.clone(), ViewState::default()).with_limits(2, 2);
        let mut fetch = Some(state.set_details(keyless));

        // Rowids are id + 100; the page plan reads them from the key column.
        for start in [0, 2] {
            let current = fetch.take().expect("fetch");
            let plan = qwerio_core::paging::PagePlan::new(&current.request).expect("plan");
            let rows = (start..start + 3)
                .map(|id| {
                    vec![
                        Value::Integer(id),
                        Value::Integer(0),
                        Value::Integer(id + 100),
                    ]
                })
                .collect();
            state.apply_page(current.ticket, Ok(plan.build_page(Vec::new(), rows)));
            fetch = state.load_more();
        }

        assert_eq!(state.released_rows(), 2);
        assert_eq!(
            state.row_identity(0),
            Some(vec![(ColumnRef::Locator, Value::Integer(102))])
        );
    }

    #[test]
    fn only_one_page_loads_at_a_time() {
        let (mut state, first) = state();
        assert!(
            state.load_more().is_none(),
            "the first page is still loading"
        );

        state.apply_page(first.ticket, Ok(page(0..3, true, &first)));
        assert!(state.load_more().is_some());
        assert!(state.load_more().is_none());
    }

    #[test]
    fn errors_are_kept_for_display() {
        let (mut state, first) = state();
        let error = DatabaseError::new(ErrorKind::Permission, "denied");

        state.apply_page(first.ticket, Err(error.clone()));

        assert_eq!(state.status(), &LoadStatus::Failed(error));
    }

    #[test]
    fn sorting_cycles_through_directions() {
        let (mut state, _) = state();

        state.toggle_sort("value");
        assert_eq!(state.view().sort, vec![SortKey::ascending("value")]);
        state.toggle_sort("value");
        assert_eq!(state.view().sort, vec![SortKey::descending("value")]);
        state.toggle_sort("value");
        assert!(state.view().sort.is_empty());
    }

    #[test]
    fn selection_follows_the_row_key_across_reloads() {
        let (mut state, first) = state();
        state.apply_page(first.ticket, Ok(page(5..8, false, &first)));
        state.select_row(Some(1));

        let refetch = state.refresh().expect("refresh");
        state.apply_page(refetch.ticket, Ok(page(4..8, false, &refetch)));

        assert_eq!(state.selected_row(), Some(2));
    }

    #[test]
    fn restored_filters_for_removed_columns_are_dropped() {
        let details = details();
        let view = ViewState {
            filters: vec![Filter::is_null("dropped"), Filter::is_null("value")],
            ..ViewState::default()
        };
        let mut state = TableBrowserState::new(details.object.clone(), view);

        state.set_details(details);

        assert_eq!(state.view().filters, vec![Filter::is_null("value")]);
    }

    #[test]
    fn inspection_reports_size_limits() {
        let (mut state, first) = state();
        state.apply_page(first.ticket, Ok(page(0..1, false, &first)));

        let inspection = state.inspect_cell(0, 1).expect("cell");
        assert_eq!(inspection.text.as_deref(), Some("0"));
        assert!(!inspection.is_truncated);

        assert_eq!(truncate_to_boundary("aé".to_owned(), 2), "a");
    }
}
