//! Browsing one table or view: a virtualized grid of database pages with
//! server-side sorting and filtering, a record inspector, relationship
//! navigation, row selection with copy and export, and inline editing:
//! cells, new rows, and deleted rows, saved together in one transaction.

mod cell_editor;
mod cell_render;
mod cell_widgets;
mod column_fit;
mod edits;
mod paging;
mod row_changes;
mod row_export;

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::table::{
    Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Div, Entity, EventEmitter,
    FocusHandle, Focusable, Hsla, InteractiveElement as _, IntoElement, Modifiers,
    ParentElement as _, Pixels, Render, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, Subscription, WeakEntity, Window, div,
};
use qwerio_app::navigation::ViewState;
use qwerio_app::row_selection::SelectionGesture;
use qwerio_app::table::{LoadStatus, TableBrowserState};
use qwerio_app::table_edit_session::TableEditSession;
use qwerio_core::catalog::{ForeignKeyInfo, ObjectDetails};
use qwerio_core::driver::DatabaseSession;
use qwerio_core::export::{ExportFormat, export_string};
use qwerio_core::filter::{Filter, FilterOperand, FilterOperator, SortDirection, SortKey};
use qwerio_core::paging::{PageNotice, PagingMode};
use qwerio_core::relationship::RelationshipOutcome;
use qwerio_core::resource::ObjectRef;
use qwerio_workspace::document::DocumentKind;
use qwerio_workspace::explorer::Location;
use qwerio_workspace::manifest::DocumentId;

use crate::actions::{AddRow, DeleteRow, DuplicateRow, EditCell, Refresh, Save, ToggleInspector};
use crate::content::ContentEvent;
use crate::filter_panel::{FilterPanel, FilterPanelEvent};
use crate::grid::{copy_text, grid_column, render_cell};
use crate::workspace_model::WorkspaceModel;

/// The grid's data source: the table browser state machine plus the session
/// that answers its page requests.
pub struct TableGridDelegate {
    browser: TableBrowserState,
    session: Option<Arc<dyn DatabaseSession>>,
    is_reloading: bool,
    owner: WeakEntity<TableView>,
    edit_session: TableEditSession,
    editing: Option<cell_widgets::ActiveEdit>,
    /// Why the last edit attempt failed, shown in the status bar.
    edit_error: Option<SharedString>,
    /// The cell under the last right click, as (grid row, browser column).
    right_clicked: Option<(usize, usize)>,
}

impl TableGridDelegate {
    fn visible(&self) -> Vec<usize> {
        self.browser.visible_columns()
    }
}

impl TableDelegate for TableGridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.visible().len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.edit_session.pending().new_row_count() + self.browser.rows().len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let visible = self.visible();
        let Some(descriptor) = visible
            .get(col_ix)
            .and_then(|index| self.browser.columns().get(*index))
        else {
            return Column::new(format!("column-missing-{col_ix}"), "");
        };

        let column = grid_column(visible[col_ix], descriptor, true);
        let sort = self
            .browser
            .view()
            .sort
            .first()
            .filter(|key| key.column == descriptor.name);

        match sort.map(|key| key.direction) {
            Some(SortDirection::Ascending) => column.sort(ColumnSort::Ascending),
            Some(SortDirection::Descending) => column.sort(ColumnSort::Descending),
            None => column,
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let visible = self.visible();
        let Some(name) = visible
            .get(col_ix)
            .and_then(|index| self.browser.columns().get(*index))
            .map(|column| column.name.clone())
        else {
            return;
        };

        if self.edit_session.is_saving() || !self.commit_edit(cx) {
            return;
        }
        let keys = match sort {
            ColumnSort::Default => Vec::new(),
            ColumnSort::Ascending => vec![SortKey::ascending(name)],
            ColumnSort::Descending => vec![SortKey::descending(name)],
        };

        let fetch = self.browser.set_sort(keys);
        self.restart(fetch, cx);
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(column) = self.visible().get(col_ix).copied() else {
            return div().into_any_element();
        };

        if self.is_editing(row_ix, column) {
            return self.render_editor(row_ix, col_ix, cx);
        }

        let content = self.render_cell_content(row_ix, col_ix, column, cx);
        cell_render::with_row_menu(content, row_ix, col_ix, column, cx)
    }

    fn fit_column(
        &mut self,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Option<Pixels> {
        self.fit_width(col_ix, window, cx)
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let row = div().id(("row", row_ix));
        let is_picked = self
            .loaded_row(row_ix)
            .is_some_and(|index| self.browser.is_row_picked(index));

        if is_picked {
            return row.bg(cx.theme().table_active);
        }

        row
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let (text, is_error): (SharedString, bool) = match self.browser.status() {
            LoadStatus::NeedsDetails | LoadStatus::Loading | LoadStatus::LoadingMore => {
                ("Loading…".into(), false)
            }
            LoadStatus::Failed(error) => (error.message.clone().into(), true),
            LoadStatus::Idle if self.browser.view().filters.is_empty() => {
                ("This table has no rows".into(), false)
            }
            LoadStatus::Idle => ("No rows match the filters".into(), false),
        };

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_4()
            .text_sm()
            .text_center()
            .text_color(if is_error {
                cx.theme().danger
            } else {
                cx.theme().muted_foreground
            })
            .child(text)
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let menu = match self.loaded_row(row_ix) {
            Some(_) => self.loaded_row_menu(row_ix, menu),
            None => menu,
        };
        let menu = if self.is_editable() {
            self.edit_menu(row_ix, menu)
        } else {
            menu
        };

        menu.separator()
            .item(self.view_item("Fit All Columns", |view, window, cx| {
                view.fit_all_columns(window, cx)
            }))
    }

    fn has_more(&self, _: &App) -> bool {
        self.browser.has_more()
    }

    fn loading(&self, _: &App) -> bool {
        matches!(self.browser.status(), LoadStatus::LoadingMore)
    }

    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        if self.edit_session.is_saving() || self.editing.is_some() || self.is_reloading {
            return;
        }
        if let Some(fetch) = self.browser.load_more() {
            self.fetch(fetch, cx);
        }
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        let visible = self.visible();

        visible
            .get(col_ix)
            .and_then(|column| self.cell_value(row_ix, *column))
            .map(copy_text)
            .unwrap_or_default()
    }
}

/// The background opacity of cells with unsaved changes.
const CHANGE_TINT_OPACITY: f32 = 0.18;

impl TableGridDelegate {
    /// A cell's value as drawn, tinted when it has unsaved changes.
    fn render_cell_content(
        &self,
        row_ix: usize,
        col_ix: usize,
        column: usize,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let cell = if self.is_checkbox_column(column) {
            self.render_checkbox(row_ix, col_ix, column, cx)
        } else if self.is_default_cell(row_ix, column) {
            cell_render::render_default_cell(row_ix, col_ix, cx)
        } else {
            match self.cell_value(row_ix, column) {
                Some(value) => render_cell(row_ix, col_ix, value, cx),
                None => return div().into_any_element(),
            }
        };

        let is_deleted = self.is_deleted_row(row_ix);
        let Some(tint) = self.cell_tint(row_ix, column, cx) else {
            return cell;
        };

        div()
            .size_full()
            .px_1()
            .rounded_sm()
            .bg(tint)
            .when(is_deleted, |this| {
                this.line_through().text_color(cx.theme().muted_foreground)
            })
            .child(cell)
            .into_any_element()
    }

    /// The background of a cell with unsaved changes: deleted, edited, or
    /// part of a new row.
    fn cell_tint(&self, row_ix: usize, column: usize, cx: &App) -> Option<Hsla> {
        let color = if self.is_deleted_row(row_ix) {
            cx.theme().danger
        } else if self.is_modified(row_ix, column) {
            cx.theme().warning
        } else if self.is_new_row(row_ix) {
            cx.theme().success
        } else {
            return None;
        };

        Some(color.opacity(CHANGE_TINT_OPACITY))
    }

    /// Cell and row editing items for the right-clicked row.
    fn edit_menu(&self, row_ix: usize, menu: PopupMenu) -> PopupMenu {
        let menu = match self.right_clicked.filter(|(row, _)| *row == row_ix) {
            Some((row, column)) => self.cell_edit_menu(row, column, menu),
            None => menu,
        };

        self.row_change_menu(row_ix, menu)
    }

    /// A menu item running `action` on the table view.
    fn view_item(
        &self,
        label: impl Into<SharedString>,
        action: impl Fn(&mut TableView, &mut Window, &mut Context<TableView>) + 'static,
    ) -> PopupMenuItem {
        let owner = self.owner.clone();
        PopupMenuItem::new(label).on_click(move |_, window, cx| {
            owner.update(cx, |view, cx| action(view, window, cx)).ok();
        })
    }

    /// Inspect, copy, and export items for a loaded row.
    fn loaded_row_menu(&self, row: usize, menu: PopupMenu) -> PopupMenu {
        let picked = self.browser.picked_row_count();
        let is_picked = self
            .loaded_row(row)
            .is_some_and(|index| self.browser.is_row_picked(index));
        let export_label = if is_picked && picked > 1 {
            format!("Export {picked} Selected Rows…")
        } else {
            "Export Row…".to_owned()
        };

        menu.item(self.view_item("Inspect Row", move |view, _, cx| view.inspect_row(row, cx)))
            .item(self.view_item("Copy Row", move |view, _, cx| view.copy_rows(Some(row), cx)))
            .item(self.view_item(export_label, move |view, window, cx| {
                view.export_row(row, window, cx)
            }))
    }

    /// Add, duplicate, and delete items for a row.
    fn row_change_menu(&self, row: usize, menu: PopupMenu) -> PopupMenu {
        let delete_label = if self.is_new_row(row) {
            "Remove New Row"
        } else if self.is_deleted_row(row) {
            "Restore Row"
        } else {
            "Delete Row"
        };
        menu.separator()
            .item(self.view_item("Add Row", |view, window, cx| view.add_row(None, window, cx)))
            .item(self.view_item("Duplicate Row", move |view, window, cx| {
                view.add_row(Some(row), window, cx)
            }))
            .item(self.view_item(delete_label, move |view, _, cx| {
                view.toggle_delete_row(row, cx)
            }))
    }

    /// Edit, NULL, and revert items for the right-clicked cell.
    fn cell_edit_menu(&self, row: usize, column: usize, menu: PopupMenu) -> PopupMenu {
        let item = |label: &'static str,
                    action: fn(
            &mut TableView,
            usize,
            usize,
            &mut Window,
            &mut Context<TableView>,
        )| {
            self.view_item(label, move |view, window, cx| {
                action(view, row, column, window, cx)
            })
        };

        if self.is_deleted_row(row) {
            return menu;
        }

        let menu = menu
            .separator()
            .item(item("Edit Cell", |view, row, column, window, cx| {
                view.edit_cell(row, column, window, cx)
            }))
            .item(item("Set Cell to NULL", |view, row, column, _, cx| {
                view.grid
                    .update(cx, |grid, cx| grid.delegate_mut().set_null(row, column, cx))
            }));

        if !self.is_modified(row, column) {
            return menu;
        }

        let revert_label = if self.is_new_row(row) {
            "Reset Cell to Default"
        } else {
            "Revert Cell"
        };

        menu.item(item(revert_label, |view, row, column, _, cx| {
            view.grid.update(cx, |grid, cx| {
                grid.delegate_mut().revert_cell(row, column, cx)
            })
        }))
    }
}

pub struct TableView {
    model: Entity<WorkspaceModel>,
    object: ObjectRef,
    grid: Entity<TableState<TableGridDelegate>>,
    filter_panel: Entity<FilterPanel>,
    inspector_open: bool,
    selected_row: Option<usize>,
    error: Option<SharedString>,
    export_job: Option<row_export::ExportJob>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ContentEvent> for TableView {}

impl TableView {
    pub fn new(
        model: Entity<WorkspaceModel>,
        object: ObjectRef,
        view: ViewState,
        inspector_open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner = cx.entity().downgrade();
        let delegate = TableGridDelegate {
            browser: TableBrowserState::new(object.clone(), view.clone()),
            session: None,
            is_reloading: false,
            owner,
            edit_session: TableEditSession::default(),
            editing: None,
            edit_error: None,
            right_clicked: None,
        };
        let grid = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .cell_selectable(true)
                .row_selectable(true)
                .col_movable(false)
        });
        let filter_panel = cx.new(|cx| FilterPanel::new(window, cx));

        let subscriptions = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.observe(&grid, |_, _, cx| {
                cx.emit(ContentEvent::Changed);
                cx.notify();
            }),
            cx.subscribe_in(&grid, window, |this, _, event: &TableEvent, window, cx| {
                this.on_grid_event(event, window, cx)
            }),
            cx.subscribe(
                &filter_panel,
                |this, _, event: &FilterPanelEvent, cx| match event {
                    FilterPanelEvent::Changed(filters) => this.set_filters(filters.clone(), cx),
                },
            ),
        ];

        let mut table = Self {
            model,
            object,
            grid,
            filter_panel,
            inspector_open,
            selected_row: None,
            error: None,
            export_job: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };

        table.load(false, view.filters, window, cx);
        table
    }

    pub fn location(&self) -> Location {
        Location::Object(self.object.clone())
    }

    pub fn filter_panel(&self) -> &Entity<FilterPanel> {
        &self.filter_panel
    }

    pub fn title(&self) -> SharedString {
        self.object.name.clone().into()
    }

    pub fn set_inspector_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.inspector_open = open;
        cx.notify();
    }

    /// The view to remember in history: filters, sort, hidden columns, and
    /// the selected row's key.
    pub fn view_state(&self, cx: &App) -> ViewState {
        self.grid.read(cx).delegate().browser.view().clone()
    }

    pub fn status_text(&self, cx: &App) -> SharedString {
        let browser = &self.grid.read(cx).delegate().browser;

        if let Some(error) =
            self.error
                .as_ref()
                .or(self.grid.read(cx).delegate().edit_error.as_ref())
        {
            return error.clone();
        }

        let loaded = browser.rows().len() + browser.released_rows();
        let mut parts = vec![if browser.has_more() {
            format!("{loaded} rows loaded, more available")
        } else {
            format!("{loaded} rows")
        }];

        if browser.paging_mode() == Some(PagingMode::Offset) {
            parts.push("no unique key: pages may shift if data changes".to_owned());
        }

        match browser.notice() {
            Some(PageNotice::OffsetLimitReached) => {
                parts.push("stopped at the paging limit; refine the filters".to_owned())
            }
            Some(PageNotice::ByteLimit) => parts.push("large rows: smaller pages".to_owned()),
            None => {}
        }

        if browser.picked_row_count() > 1 {
            parts.push(format!("{} selected", browser.picked_row_count()));
        }

        if browser.released_rows() > 0 {
            parts.push(format!(
                "first {} rows released from memory",
                browser.released_rows()
            ));
        }

        parts.join(" · ").into()
    }

    fn load(
        &mut self,
        force: bool,
        filters: Vec<Filter>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.grid.update(cx, |grid, cx| {
            grid.delegate_mut().is_reloading = true;
            cx.notify();
        });
        self.error = None;
        let object = self.object.clone();
        let session =
            self.model
                .read(cx)
                .browse_session(object.connection, object.database.clone(), cx);
        let describe = self
            .model
            .update(cx, |model, cx| model.describe(object, force, cx));

        cx.spawn_in(window, async move |this, cx| {
            let loaded = match session.await {
                Ok(session) => describe.await.map(|details| (session, details)),
                Err(error) => Err(error),
            };

            this.update_in(cx, |this, window, cx| match loaded {
                Ok((session, details)) => this.show(session, details, filters, window, cx),
                Err(error) => {
                    this.grid.update(cx, |grid, cx| {
                        grid.delegate_mut().is_reloading = false;
                        grid.delegate_mut().browser.details_failed(
                            qwerio_core::error::DatabaseError::new(
                                qwerio_core::error::ErrorKind::Other,
                                error.to_string(),
                            ),
                        );
                        cx.notify();
                    });
                    this.error = Some(error);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn show(
        &mut self,
        session: Arc<dyn DatabaseSession>,
        details: Arc<ObjectDetails>,
        filters: Vec<Filter>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.object = details.object.clone();
        let columns = details
            .columns
            .iter()
            .map(|column| SharedString::from(column.name.clone()))
            .collect();

        self.grid.update(cx, |grid, cx| {
            let delegate = grid.delegate_mut();
            delegate.session = Some(session);
            delegate.is_reloading = false;
            delegate
                .edit_session
                .edit_pending(|edits| edits.retain_columns(&details));
            let fetch = delegate.browser.set_details(details);
            delegate.fetch(fetch, cx);
            grid.refresh(cx);
        });

        let restored = filters
            .into_iter()
            .filter(|filter| {
                self.details(cx)
                    .is_some_and(|details| details.column(&filter.column).is_some())
            })
            .collect();
        self.filter_panel.update(cx, |panel, cx| {
            panel.set_columns(columns, restored, window, cx)
        });
        cx.emit(ContentEvent::Changed);
        cx.notify();
    }

    fn details<'a>(&self, cx: &'a App) -> Option<&'a Arc<ObjectDetails>> {
        self.grid.read(cx).delegate().browser.details()
    }

    fn set_filters(&mut self, filters: Vec<Filter>, cx: &mut Context<Self>) {
        self.grid.update(cx, |grid, cx| {
            let delegate = grid.delegate_mut();
            if delegate.edit_session.is_saving() || !delegate.commit_edit(cx) {
                return;
            }
            let fetch = delegate.browser.set_filters(filters);
            grid.delegate_mut().restart(fetch, cx);
        });
    }

    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_saving(cx) {
            return;
        }
        let committed = self
            .grid
            .update(cx, |grid, cx| grid.delegate_mut().commit_edit(cx));
        if !committed {
            return;
        }
        let filters = self.view_state(cx).filters;
        self.load(true, filters, window, cx);
    }

    fn on_grid_event(&mut self, event: &TableEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.track_editing(event, window, cx);

        let row = match event {
            TableEvent::SelectRow(row) | TableEvent::SelectCell(row, _) => Some(*row),
            TableEvent::DoubleClickedCell(row, _) if self.is_editable(cx) => Some(*row),
            TableEvent::DoubleClickedRow(row) | TableEvent::DoubleClickedCell(row, _) => {
                self.inspector_open = true;
                Some(*row)
            }
            _ => None,
        };

        let gesture = match event {
            TableEvent::SelectRow(_) | TableEvent::SelectCell(..) => {
                Some(selection_gesture(window.modifiers(), cx))
            }
            _ => None,
        };

        if let Some(row) = row {
            self.selected_row = Some(row);
            self.grid.update(cx, |grid, cx| {
                let delegate = grid.delegate_mut();
                let loaded = delegate.loaded_row(row);
                delegate.browser.select_row(loaded);

                if let Some(gesture) = gesture {
                    delegate.browser.pick_row(loaded, gesture);
                }

                // A modified row header click also selects the row in the
                // table itself, which would keep a row Ctrl took out of the
                // picked rows highlighted.
                let is_unpicked = loaded.is_none_or(|index| !delegate.browser.is_row_picked(index));
                if grid.selected_row() == Some(row) && is_unpicked {
                    grid.clear_selection(cx);
                }
            });
            cx.notify();
        }
    }

    /// Fits every column to its header and loaded rows, as a double-click on
    /// each resize handle does.
    fn fit_all_columns(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.grid.update(cx, |grid, cx| {
            for col_ix in 0..grid.delegate().columns_count(cx) {
                grid.fit_col(col_ix, window, cx);
            }
        });
    }

    fn inspect_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.selected_row = Some(row);
        self.inspector_open = true;
        cx.notify();
    }

    /// Copies loaded rows (or one row) as tab-separated text. Only rows that
    /// are loaded are copied; use Export for the whole table.
    fn copy_rows(&mut self, row: Option<usize>, cx: &mut Context<Self>) {
        let delegate = self.grid.read(cx).delegate();
        let browser = &delegate.browser;
        let rows = match row {
            Some(row) => delegate
                .loaded_row(row)
                .and_then(|row| browser.rows().get(row))
                .cloned()
                .into_iter()
                .collect::<Vec<_>>(),
            None => browser.rows().to_vec(),
        };
        let Some(target) = delegate.export_target() else {
            return;
        };
        let text = export_string(ExportFormat::Tsv, &target, browser.columns(), &rows);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn follow(
        &mut self,
        foreign_key: ForeignKeyInfo,
        referencing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delegate = self.grid.read(cx).delegate();
        let Some(row) = self.selected_row.and_then(|row| delegate.loaded_row(row)) else {
            return;
        };

        let browser = &delegate.browser;
        let outcome = if referencing {
            browser.follow_referencing(row, &foreign_key)
        } else {
            browser.follow_reference(row, &foreign_key)
        };

        match outcome {
            Some(RelationshipOutcome::Target(target)) => cx.emit(ContentEvent::Navigate {
                location: Location::Object(target.object),
                view: ViewState {
                    filters: target.filters,
                    ..ViewState::default()
                },
                new_tab: false,
            }),
            Some(RelationshipOutcome::NullKey) => window.push_notification(
                Notification::info("No related row: part of the key is NULL"),
                cx,
            ),
            Some(RelationshipOutcome::MissingColumns(columns)) => window.push_notification(
                Notification::warning(format!(
                    "The key columns {} are missing; refresh the table",
                    columns.join(", ")
                )),
                cx,
            ),
            None => {}
        }
    }

    fn filter_by_cell(&mut self, column: usize, cx: &mut Context<Self>) {
        let delegate = self.grid.read(cx).delegate();
        let browser = &delegate.browser;
        let Some(row) = self.selected_row.and_then(|row| delegate.loaded_row(row)) else {
            return;
        };
        let (Some(descriptor), Some(value)) = (
            browser.columns().get(column),
            browser.rows().get(row).and_then(|row| row.get(column)),
        ) else {
            return;
        };

        let filter = if value.is_null() {
            Filter::is_null(descriptor.name.clone())
        } else {
            Filter::new(
                descriptor.name.clone(),
                FilterOperator::Equals,
                FilterOperand::Single(value.clone()),
            )
        };

        self.filter_panel
            .update(cx, |panel, cx| panel.add_filter(filter, cx));
    }

    fn toggle_column(&mut self, name: String, cx: &mut Context<Self>) {
        self.grid.update(cx, |grid, cx| {
            let hidden = grid
                .delegate()
                .browser
                .view()
                .hidden_columns
                .contains(&name);
            grid.delegate_mut()
                .browser
                .set_column_hidden(&name, !hidden);
            grid.refresh(cx);
        });
        cx.emit(ContentEvent::Changed);
    }

    fn columns_menu(
        this: WeakEntity<Self>,
        columns: Vec<(String, bool)>,
        menu: PopupMenu,
    ) -> PopupMenu {
        columns.into_iter().fold(menu, |menu, (name, visible)| {
            let this = this.clone();
            let toggled = name.clone();
            menu.item(
                PopupMenuItem::new(name)
                    .checked(visible)
                    .on_click(move |_, _, cx| {
                        this.update(cx, |view, cx| view.toggle_column(toggled.clone(), cx))
                            .ok();
                    }),
            )
        })
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let browser = &self.grid.read(cx).delegate().browser;
        let columns: Vec<(String, bool)> = browser
            .columns()
            .iter()
            .map(|column| {
                let visible = !browser.view().hidden_columns.contains(&column.name);
                (column.name.clone(), visible)
            })
            .collect();
        let loaded = browser.rows().len();
        let this = cx.entity().downgrade();

        h_flex()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("refresh")
                    .ghost()
                    .small()
                    .icon(IconName::RefreshCw)
                    .accessibility_label("Refresh")
                    .tooltip("Refresh")
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
            .child(
                Button::new("columns")
                    .ghost()
                    .small()
                    .label("Columns")
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        Self::columns_menu(this.clone(), columns.clone(), menu)
                    }),
            )
            .child(
                Button::new("copy-loaded")
                    .ghost()
                    .small()
                    .icon(IconName::Copy)
                    .label(format!("Copy {loaded} Loaded Rows"))
                    .disabled(loaded == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.copy_rows(None, cx))),
            )
            .child(
                Button::new("export")
                    .ghost()
                    .small()
                    .icon(gpui_kit::component::Icon::new(
                        gpui_kit::assets::IconName::Download,
                    ))
                    .label("Export…")
                    .tooltip("Export selected or all rows as CSV, JSON, or SQL INSERT")
                    .disabled(self.is_exporting())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_export_dialog(window, cx)),
                    ),
            )
            .child(self.render_documents_menu(cx))
            .child(div().flex_1())
            .children(self.render_row_controls(cx))
            .children(self.render_edit_controls(cx))
            .child(
                Button::new("inspector")
                    .ghost()
                    .small()
                    .icon(IconName::PanelRight)
                    .accessibility_label("Inspector")
                    .tooltip("Inspector")
                    .toggled(self.inspector_open)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.inspector_open = !this.inspector_open;
                        cx.notify();
                    })),
            )
    }

    /// Queries and notes linked to this table.
    fn render_documents_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let documents: Vec<(DocumentId, String)> = self
            .model
            .read(cx)
            .manifest()
            .documents_for_object(&self.object)
            .into_iter()
            .map(|document| (document.id, document.file_name().to_owned()))
            .collect();
        let label = format!("Documents ({})", documents.len());
        let this = cx.entity().downgrade();
        let object = self.object.clone();

        Button::new("documents")
            .ghost()
            .small()
            .icon(Icon::new(gpui_kit::assets::IconName::Link))
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let menu = documents.iter().fold(menu, |menu, (document, name)| {
                    let this = this.clone();
                    let location = Location::Document(*document);
                    menu.item(PopupMenuItem::new(name.clone()).on_click(move |_, _, cx| {
                        let location = location.clone();
                        this.update(cx, |_, cx| {
                            cx.emit(ContentEvent::Navigate {
                                location,
                                view: ViewState::default(),
                                new_tab: false,
                            })
                        })
                        .ok();
                    }))
                });
                let menu = if documents.is_empty() {
                    menu
                } else {
                    menu.separator()
                };
                let new_item = |label: &'static str, kind: DocumentKind| {
                    let this = this.clone();
                    let object = object.clone();
                    PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        let object = Some(object.clone());
                        this.update(cx, |_, cx| {
                            cx.emit(ContentEvent::NewDocument { kind, object })
                        })
                        .ok();
                    })
                };

                menu.item(new_item("New Query for Table", DocumentKind::Sql))
                    .item(new_item("New Note for Table", DocumentKind::Markdown))
            })
    }

    fn render_inspector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let delegate = self.grid.read(cx).delegate();
        let browser = &delegate.browser;
        let details = browser.details().cloned();
        let is_new_row = self
            .selected_row
            .is_some_and(|row| delegate.is_new_row(row));
        let row = self
            .selected_row
            .and_then(|row| delegate.loaded_row(row))
            .filter(|row| *row < browser.rows().len());

        let body = match (details, row) {
            (Some(details), Some(row)) => {
                let fields: Vec<_> = (0..browser.columns().len())
                    .filter_map(|column| {
                        browser
                            .inspect_cell(row, column)
                            .map(|inspection| (column, inspection))
                    })
                    .collect();

                let field_rows = fields.into_iter().map(|(column, inspection)| {
                    let value_text: SharedString = match (&inspection.text, inspection.is_null) {
                        (_, true) => "NULL".into(),
                        (Some(text), false) if inspection.is_truncated => {
                            format!("{text}… ({} bytes total)", inspection.total_bytes).into()
                        }
                        (Some(text), false) => text.clone().into(),
                        (None, false) => format!("<{}>", inspection.storage).into(),
                    };
                    let copy_value = inspection.text.clone().unwrap_or_default();

                    v_flex()
                        .gap_0p5()
                        .py_1()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "{} · {}",
                                            inspection.column.name, inspection.storage
                                        )),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("copy-field-{column}")))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Copy)
                                        .accessibility_label("Copy value")
                                        .tooltip("Copy value")
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copy_value.clone(),
                                            ));
                                        }),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "filter-field-{column}"
                                    )))
                                    .ghost()
                                    .xsmall()
                                    .icon(gpui_kit::component::Icon::new(
                                        gpui_kit::assets::IconName::Funnel,
                                    ))
                                    .accessibility_label("Filter by this value")
                                    .tooltip("Filter by this value")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.filter_by_cell(column, cx)
                                    })),
                                ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .when(inspection.is_null, |this| {
                                    this.italic().text_color(cx.theme().muted_foreground)
                                })
                                .child(value_text),
                        )
                });

                let references = details.foreign_keys.iter().enumerate().map(|(index, key)| {
                    let key = key.clone();
                    let label =
                        format!("{} ({})", key.referenced_table.name, key.columns.join(", "));
                    Button::new(SharedString::from(format!("reference-{index}")))
                        .ghost()
                        .small()
                        .justify_start()
                        .icon(gpui_kit::component::Icon::new(
                            gpui_kit::assets::IconName::ArrowUpRight,
                        ))
                        .label(label)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.follow(key.clone(), false, window, cx)
                        }))
                });

                let referencing =
                    details
                        .referencing_keys
                        .iter()
                        .enumerate()
                        .map(|(index, key)| {
                            let key = key.clone();
                            let label = format!("{} ({})", key.table.name, key.columns.join(", "));
                            Button::new(SharedString::from(format!("referencing-{index}")))
                                .ghost()
                                .small()
                                .justify_start()
                                .icon(gpui_kit::component::Icon::new(
                                    gpui_kit::assets::IconName::CornerDownRight,
                                ))
                                .label(label)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.follow(key.clone(), true, window, cx)
                                }))
                        });

                v_flex()
                    .gap_1()
                    .children(field_rows)
                    .when(!details.foreign_keys.is_empty(), |this| {
                        this.child(section_label("References", cx))
                            .children(references)
                    })
                    .when(!details.referencing_keys.is_empty(), |this| {
                        this.child(section_label("Referenced by", cx))
                            .children(referencing)
                    })
                    .into_any_element()
            }
            _ => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(if is_new_row {
                    "This row is new: edit its values in the grid and save to insert it."
                } else {
                    "Select a row to see its values and related records."
                })
                .into_any_element(),
        };

        v_flex()
            .id("inspector")
            .w_80()
            .flex_shrink_0()
            .h_full()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .overflow_y_scroll()
            .p_3()
            .gap_2()
            .child(div().font_semibold().child("Record"))
            .child(body)
    }
}

/// Shift extends the selected rows to a range; Ctrl (Cmd on macOS) adds or
/// removes one row.
fn selection_gesture(modifiers: Modifiers, cx: &App) -> SelectionGesture {
    if modifiers.shift {
        SelectionGesture::Extend
    } else if modifiers.secondary(cx) {
        SelectionGesture::Toggle
    } else {
        SelectionGesture::Replace
    }
}

fn section_label(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .pt_3()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

impl Focusable for TableView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TableView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("table-view")
            .track_focus(&self.focus_handle)
            .size_full()
            .min_w_0()
            .min_h_0()
            .on_action(cx.listener(|this, _: &Refresh, window, cx| this.refresh(window, cx)))
            .on_action(
                cx.listener(|this, _: &Save, window, cx| this.save_edits(window, cx).detach()),
            )
            .on_action(
                cx.listener(|this, _: &EditCell, window, cx| this.edit_selected_cell(window, cx)),
            )
            .on_action(cx.listener(|this, _: &AddRow, window, cx| this.add_row(None, window, cx)))
            .on_action(cx.listener(|this, _: &DuplicateRow, window, cx| {
                this.duplicate_selected_row(window, cx)
            }))
            .on_action(cx.listener(|this, _: &DeleteRow, _, cx| this.delete_selected_row(cx)))
            .on_action(cx.listener(|this, _: &ToggleInspector, _, cx| {
                this.inspector_open = !this.inspector_open;
                cx.notify();
            }))
            .child(self.render_toolbar(cx))
            .children(self.render_export_progress(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(DataTable::new(&self.grid).stripe(true).bordered(false)),
                    )
                    .when(self.inspector_open, |this| {
                        this.child(self.render_inspector(cx))
                    }),
            )
    }
}
