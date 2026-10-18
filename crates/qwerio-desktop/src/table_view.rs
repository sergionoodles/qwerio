//! Browsing one table or view: a virtualized grid of database pages with
//! server-side sorting and filtering, a record inspector, relationship
//! navigation, copy, and full export.

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
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window, div,
};
use qwerio_app::navigation::ViewState;
use qwerio_app::services::export_table;
use qwerio_app::table::{LoadStatus, PageFetch, TableBrowserState};
use qwerio_core::catalog::{ForeignKeyInfo, ObjectDetails};
use qwerio_core::driver::DatabaseSession;
use qwerio_core::export::{ExportFormat, delimited_string};
use qwerio_core::filter::{Filter, FilterOperand, FilterOperator, SortDirection, SortKey};
use qwerio_core::paging::{PageNotice, PagingMode};
use qwerio_core::relationship::RelationshipOutcome;
use qwerio_core::resource::ObjectRef;
use qwerio_workspace::document::DocumentKind;
use qwerio_workspace::explorer::Location;
use qwerio_workspace::manifest::DocumentId;

use crate::actions::{Refresh, ToggleInspector};
use crate::content::ContentEvent;
use crate::filter_panel::{FilterPanel, FilterPanelEvent};
use crate::grid::{copy_text, grid_column, render_cell};
use crate::workspace_model::WorkspaceModel;

/// The grid's data source: the table browser state machine plus the session
/// that answers its page requests.
pub struct TableGridDelegate {
    browser: TableBrowserState,
    session: Option<Arc<dyn DatabaseSession>>,
    owner: WeakEntity<TableView>,
}

impl TableGridDelegate {
    fn visible(&self) -> Vec<usize> {
        self.browser.visible_columns()
    }

    /// Fetches a page; results for superseded requests are ignored by the
    /// browser state.
    fn fetch(&mut self, fetch: PageFetch, cx: &mut Context<TableState<Self>>) {
        let Some(session) = self.session.clone() else {
            return;
        };

        cx.spawn(async move |table, cx| {
            let result = session.fetch_page(fetch.request).await;

            table
                .update(cx, |table, cx| {
                    let columns_before = table.delegate().browser.columns().len();
                    let applied = table
                        .delegate_mut()
                        .browser
                        .apply_page(fetch.ticket, result);

                    if applied && table.delegate().browser.columns().len() != columns_before {
                        table.refresh(cx);
                    }

                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn restart(&mut self, fetch: Option<PageFetch>, cx: &mut Context<TableState<Self>>) {
        if let Some(fetch) = fetch {
            self.fetch(fetch, cx);
        }

        cx.notify();
    }
}

impl TableDelegate for TableGridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.visible().len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.browser.rows().len()
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
        let visible = self.visible();

        match visible.get(col_ix).and_then(|column| {
            self.browser
                .rows()
                .get(row_ix)
                .and_then(|row| row.get(*column))
        }) {
            Some(value) => render_cell(value, cx),
            None => div().into_any_element(),
        }
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
        let owner = self.owner.clone();
        let inspect = {
            let owner = owner.clone();
            PopupMenuItem::new("Inspect Row").on_click(move |_, _, cx| {
                owner
                    .update(cx, |view, cx| view.inspect_row(row_ix, cx))
                    .ok();
            })
        };
        let copy = PopupMenuItem::new("Copy Row").on_click(move |_, _, cx| {
            owner
                .update(cx, |view, cx| view.copy_rows(Some(row_ix), cx))
                .ok();
        });

        menu.item(inspect).item(copy)
    }

    fn has_more(&self, _: &App) -> bool {
        self.browser.has_more()
    }

    fn loading(&self, _: &App) -> bool {
        matches!(self.browser.status(), LoadStatus::LoadingMore)
    }

    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        if let Some(fetch) = self.browser.load_more() {
            self.fetch(fetch, cx);
        }
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        let visible = self.visible();

        visible
            .get(col_ix)
            .and_then(|column| {
                self.browser
                    .rows()
                    .get(row_ix)
                    .and_then(|row| row.get(*column))
            })
            .map(copy_text)
            .unwrap_or_default()
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
            owner,
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
            cx.subscribe(&grid, |this, _, event: &TableEvent, cx| {
                this.on_grid_event(event, cx)
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

        if let Some(error) = &self.error {
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
            let fetch = grid.delegate_mut().browser.set_filters(filters);
            grid.delegate_mut().restart(fetch, cx);
        });
    }

    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let filters = self.view_state(cx).filters;
        self.load(true, filters, window, cx);
    }

    fn on_grid_event(&mut self, event: &TableEvent, cx: &mut Context<Self>) {
        let row = match event {
            TableEvent::SelectRow(row) | TableEvent::SelectCell(row, _) => Some(*row),
            TableEvent::DoubleClickedRow(row) | TableEvent::DoubleClickedCell(row, _) => {
                self.inspector_open = true;
                Some(*row)
            }
            _ => None,
        };

        if let Some(row) = row {
            self.selected_row = Some(row);
            self.grid.update(cx, |grid, _| {
                grid.delegate_mut().browser.select_row(Some(row))
            });
            cx.notify();
        }
    }

    fn inspect_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.selected_row = Some(row);
        self.inspector_open = true;
        cx.notify();
    }

    /// Copies loaded rows (or one row) as tab-separated text. Only rows that
    /// are loaded are copied; use Export for the whole table.
    fn copy_rows(&mut self, row: Option<usize>, cx: &mut Context<Self>) {
        let browser = &self.grid.read(cx).delegate().browser;
        let rows = match row {
            Some(row) => browser
                .rows()
                .get(row)
                .cloned()
                .into_iter()
                .collect::<Vec<_>>(),
            None => browser.rows().to_vec(),
        };
        let text = delimited_string(ExportFormat::Tsv, browser.columns(), &rows);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let browser = &self.grid.read(cx).delegate().browser;
        let (Some(details), Some(session)) = (
            browser.details().cloned(),
            self.grid.read(cx).delegate().session.clone(),
        ) else {
            return;
        };

        let view = browser.view().clone();
        let request = qwerio_core::paging::PageRequest {
            filters: view.filters,
            sort: view.sort,
            ..qwerio_core::paging::PageRequest::first(details)
        };
        let directory = self.model.read(cx).root().clone();
        let file_name = format!(
            "{}.csv",
            qwerio_app::templates::file_stem(&self.object.name)
        );
        let prompt = cx.prompt_for_new_path(&directory, Some(&file_name));

        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };

            let export = cx.background_spawn(async move {
                let mut file = std::fs::File::create(&path).map_err(|error| error.to_string())?;
                export_table(session.as_ref(), request, ExportFormat::Csv, &mut file)
                    .await
                    .map_err(|error| error.message)
                    .map(|summary| (summary, path))
            });

            let result = export.await;

            cx.update(|window, cx| {
                let notification = match result {
                    Ok((summary, path)) if summary.hit_limit => Notification::warning(format!(
                        "Exported the first {} rows to {} (export limit reached)",
                        summary.rows,
                        path.display()
                    )),
                    Ok((summary, path)) => Notification::success(format!(
                        "Exported {} rows to {}",
                        summary.rows,
                        path.display()
                    )),
                    Err(error) => Notification::error(format!("Export failed: {error}")),
                };
                window.push_notification(notification, cx);
            })
            .ok();
        })
        .detach();
    }

    fn follow(
        &mut self,
        foreign_key: ForeignKeyInfo,
        referencing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.selected_row else {
            return;
        };

        let browser = &self.grid.read(cx).delegate().browser;
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
        let browser = &self.grid.read(cx).delegate().browser;
        let Some(row) = self.selected_row else {
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
                    .label("Export All…")
                    .tooltip("Export every matching row to a CSV file")
                    .on_click(cx.listener(|this, _, window, cx| this.export(window, cx))),
            )
            .child(self.render_documents_menu(cx))
            .child(div().flex_1())
            .child(
                Button::new("inspector")
                    .ghost()
                    .small()
                    .icon(IconName::PanelRight)
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
        let browser = &self.grid.read(cx).delegate().browser;
        let details = browser.details().cloned();
        let row = self.selected_row.filter(|row| *row < browser.rows().len());

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
                .child("Select a row to see its values and related records.")
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
            .on_action(cx.listener(|this, _: &ToggleInspector, _, cx| {
                this.inspector_open = !this.inspector_open;
                cx.notify();
            }))
            .child(self.render_toolbar(cx))
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
