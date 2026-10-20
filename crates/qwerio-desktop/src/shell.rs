//! The workspace window: toolbar with navigation and breadcrumbs, a sidebar
//! with places and contextual filters, tabs with one optional split, and a
//! status bar. Tab content is an explorer, a table, or a document.

mod schema_changes;
mod toolbar;
mod unsaved_edits;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::dialog::DialogClose;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _,
    WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div, px,
};
use qwerio_app::navigation::ViewState;
use qwerio_app::tabs::{Pane, SplitAxis, TabId, TabSet};
use qwerio_app::templates;
use qwerio_core::resource::ObjectRef;
use qwerio_workspace::document::DocumentKind;
use qwerio_workspace::explorer::{Location, ScratchId, breadcrumbs, enclosing_container};
use qwerio_workspace::manifest::{DocumentId, ExecutionBinding, Placement};
use qwerio_workspace::{Workspace, WorkspaceError};
use serde::{Deserialize, Serialize};

use crate::actions::*;
use crate::connection_dialog;
use crate::content::{ContentEvent, entry_icon};
use crate::document_view::{DocumentSource, DocumentView};
use crate::explorer_view::{ExplorerLayout, ExplorerView};
use crate::table_view::TableView;
use crate::workspace_model::{WorkspaceEvent, WorkspaceModel, error_text};

/// Below this width the sidebar and inspector start hidden.
const NARROW_WIDTH: f32 = 760.;
const UI_STATE_SAVE_DELAY: Duration = Duration::from_millis(800);
const SIDEBAR_WIDTH: f32 = 240.;
const SIDEBAR_MIN_WIDTH: f32 = 180.;
const SIDEBAR_MAX_WIDTH: f32 = 420.;

/// Folders new documents are written to, by kind.
fn document_folder(kind: DocumentKind) -> &'static str {
    match kind {
        DocumentKind::Sql => "queries",
        DocumentKind::Markdown | DocumentKind::Text => "notes",
    }
}

/// Saved per workspace in local state; never in the shared manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct UiState {
    tabs: TabSet,
    favorites: Vec<Location>,
    sidebar_open: bool,
    inspector_open: bool,
    explorer_layout: ExplorerLayout,
}

#[derive(Clone)]
enum TabContent {
    Explorer(Entity<ExplorerView>),
    Table(Entity<TableView>),
    Document(Entity<DocumentView>),
}

impl TabContent {
    fn location(&self, cx: &App) -> Location {
        match self {
            Self::Explorer(view) => view.read(cx).location().clone(),
            Self::Table(view) => view.read(cx).location(),
            Self::Document(view) => view.read(cx).location(),
        }
    }

    fn title(&self, cx: &App) -> SharedString {
        match self {
            Self::Explorer(view) => view.read(cx).title(cx),
            Self::Table(view) => view.read(cx).title(),
            Self::Document(view) => view.read(cx).title(cx),
        }
    }

    fn status(&self, cx: &App) -> SharedString {
        match self {
            Self::Explorer(view) => view.read(cx).status_text(),
            Self::Table(view) => view.read(cx).status_text(cx),
            Self::Document(view) => view.read(cx).status_text(cx),
        }
    }

    fn element(&self) -> AnyElement {
        match self {
            Self::Explorer(view) => view.clone().into_any_element(),
            Self::Table(view) => view.clone().into_any_element(),
            Self::Document(view) => view.clone().into_any_element(),
        }
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Explorer(view) => window.focus(&view.focus_handle(cx), cx),
            Self::Table(view) => window.focus(&view.focus_handle(cx), cx),
            Self::Document(view) => window.focus(&view.focus_handle(cx), cx),
        }
    }
}

struct TabSlot {
    content: TabContent,
    _subscription: Subscription,
}

pub struct WorkspaceShell {
    model: Entity<WorkspaceModel>,
    tabs: TabSet,
    slots: HashMap<TabId, TabSlot>,
    documents: HashMap<DocumentId, Entity<DocumentView>>,
    /// Temporary queries live only in this session.
    scratches: HashMap<ScratchId, Entity<DocumentView>>,
    favorites: Vec<Location>,
    sidebar_open: bool,
    sidebar_open_when_narrow: bool,
    inspector_open: bool,
    explorer_layout: ExplorerLayout,
    entry_filter: Entity<InputState>,
    palette: Entity<CommandState>,
    save_task: Option<Task<()>>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceShell {
    pub fn new(model: Entity<WorkspaceModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let entry_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter items"));
        let palette = cx.new(|cx| CommandState::new(window, cx));

        let subscriptions = vec![
            cx.subscribe_in(
                &model,
                window,
                |this, _, event: &WorkspaceEvent, window, cx| {
                    this.on_workspace_event(event, window, cx)
                },
            ),
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.subscribe(&entry_filter, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.apply_entry_filter(cx);
                }
            }),
        ];

        let mut shell = Self {
            model,
            tabs: TabSet::new(Location::Root),
            slots: HashMap::new(),
            documents: HashMap::new(),
            scratches: HashMap::new(),
            favorites: Vec::new(),
            sidebar_open: true,
            sidebar_open_when_narrow: false,
            inspector_open: false,
            explorer_layout: ExplorerLayout::Details,
            entry_filter,
            palette,
            save_task: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };

        shell.sync_all(window, cx);
        if let Some(content) = shell.focused_content() {
            content.focus(window, cx);
        }
        shell.restore_ui_state(window, cx);
        shell
    }

    pub fn title(&self, cx: &App) -> SharedString {
        format!("{} — Qwerio", self.model.read(cx).manifest().name).into()
    }

    pub fn model(&self) -> Entity<WorkspaceModel> {
        self.model.clone()
    }

    /// Ends console sessions and saves UI state before the shell goes away.
    pub fn shut_down(&mut self, cx: &mut Context<Self>) {
        self.save_task = None;

        for document in self.documents.values().chain(self.scratches.values()) {
            document.update(cx, |document, cx| document.shut_down(cx));
        }
    }

    // --- UI state ------------------------------------------------------------

    fn ui_state(&self) -> UiState {
        UiState {
            tabs: self.tabs.clone(),
            favorites: self.favorites.clone(),
            sidebar_open: self.sidebar_open,
            inspector_open: self.inspector_open,
            explorer_layout: self.explorer_layout,
        }
    }

    fn restore_ui_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let storage = self.model.read(cx).storage();
        let load = cx.background_spawn(async move {
            storage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .ui_state::<UiState>()
        });

        cx.spawn_in(window, async move |this, cx| {
            let state = match load.await {
                Ok(Some(state)) => state,
                Ok(None) => return,
                Err(error) => {
                    cx.update(|window, cx| {
                        window.push_notification(
                            Notification::error(format!(
                                "Could not restore workspace state: {error}"
                            )),
                            cx,
                        )
                    })
                    .ok();
                    return;
                }
            };

            this.update_in(cx, |this, window, cx| {
                let manifest = Arc::clone(this.model.read(cx).manifest());
                let scratches = this.scratch_ids();
                let exists = |location: &Location| location_exists(&manifest, &scratches, location);

                let mut tabs = state.tabs;
                tabs.retain_locations(exists, Location::Root);
                this.tabs = tabs;
                this.favorites = state
                    .favorites
                    .into_iter()
                    .filter(|location| exists(location))
                    .collect();
                this.sidebar_open = state.sidebar_open;
                this.inspector_open = state.inspector_open;
                this.explorer_layout = state.explorer_layout;
                this.slots.clear();
                this.sync_all(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn schedule_ui_save(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UI_STATE_SAVE_DELAY).await;
            this.update(cx, |this, cx| {
                let running = this.save_task.take();
                this.save_ui_state_now(cx);
                this.save_task = running;
            })
            .ok();
        }));
    }

    fn save_ui_state_now(&mut self, cx: &mut Context<Self>) {
        let save = self.persist_ui_state(cx);
        let model = self.model.clone();
        cx.spawn(async move |_, cx| {
            if let Err(error) = save.await {
                model.update(cx, |_, cx| cx.emit(WorkspaceEvent::SaveFailed(error)));
            }
        })
        .detach();
    }

    pub(super) fn persist_ui_state(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), SharedString>> {
        self.save_task = None;
        self.capture_all_views(cx);
        let state = self.ui_state();
        let storage = self.model.read(cx).storage();
        cx.background_spawn(async move {
            storage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .save_ui_state(&state)
                .map_err(|error| error_text(&error))
        })
    }

    // --- Tab content -----------------------------------------------------------

    fn sync_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<TabId> = self.tabs.tabs().iter().map(|tab| tab.id).collect();
        self.slots.retain(|id, _| ids.contains(id));

        for id in ids {
            self.sync_slot(id, window, cx);
        }
    }

    /// Makes a tab's content view match its history's current location.
    fn sync_slot(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.tab(id) else {
            return;
        };

        let entry = tab.history.current().clone();

        if self
            .slots
            .get(&id)
            .is_some_and(|slot| slot.content.location(cx) == entry.location)
        {
            return;
        }

        let content = match &entry.location {
            Location::Object(object) => {
                let inspector_open = self.inspector_open && !is_narrow(window);
                let model = self.model.clone();
                let object = object.clone();
                let view = entry.view.clone();
                TabContent::Table(
                    cx.new(|cx| TableView::new(model, object, view, inspector_open, window, cx)),
                )
            }
            Location::Document(document) => {
                TabContent::Document(self.document_view(*document, window, cx))
            }
            Location::Scratch(scratch) => match self.scratches.get(scratch) {
                Some(view) => TabContent::Document(view.clone()),
                None => return,
            },
            container => {
                let model = self.model.clone();
                let location = container.clone();
                let layout = self.explorer_layout;
                let filter = self.entry_filter.read(cx).value().to_string();
                let explorer = cx.new(|cx| {
                    let mut explorer = ExplorerView::new(model, location, layout, cx);
                    explorer.set_filter(&filter, cx);
                    explorer
                });
                TabContent::Explorer(explorer)
            }
        };

        let subscription = match &content {
            TabContent::Explorer(view) => cx.subscribe_in(
                view,
                window,
                move |this, _, event: &ContentEvent, window, cx| {
                    this.on_content_event(event.clone(), window, cx)
                },
            ),
            TabContent::Table(view) => cx.subscribe_in(
                view,
                window,
                move |this, _, event: &ContentEvent, window, cx| {
                    this.on_content_event(event.clone(), window, cx)
                },
            ),
            TabContent::Document(view) => cx.subscribe_in(
                view,
                window,
                move |this, _, event: &ContentEvent, window, cx| {
                    this.on_content_event(event.clone(), window, cx)
                },
            ),
        };

        self.slots.insert(
            id,
            TabSlot {
                content,
                _subscription: subscription,
            },
        );
    }

    /// Document views are shared by tabs and outlive navigation, so unsaved
    /// edits are never lost by switching away.
    fn document_view(
        &mut self,
        document: DocumentId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<DocumentView> {
        if let Some(view) = self.documents.get(&document) {
            return view.clone();
        }

        let model = self.model.clone();
        let view = cx
            .new(|cx| DocumentView::new(model, DocumentSource::Saved(document), None, window, cx));
        self.documents.insert(document, view.clone());
        view
    }

    fn scratch_ids(&self) -> HashSet<ScratchId> {
        self.scratches.keys().copied().collect()
    }

    /// Opens a temporary query, suggesting the current connection as target.
    fn new_temporary_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let suggested =
            match enclosing_container(self.model.read(cx).manifest(), &self.focused_location()) {
                Location::Container { connection, path } => Some(ExecutionBinding {
                    connection,
                    database: path.database,
                }),
                _ => None,
            };
        let scratch = ScratchId::new();
        let model = self.model.clone();
        let view = cx.new(|cx| {
            DocumentView::new(
                model,
                DocumentSource::Scratch(scratch),
                suggested,
                window,
                cx,
            )
        });
        self.scratches.insert(scratch, view);
        self.navigate(
            Location::Scratch(scratch),
            ViewState::default(),
            true,
            window,
            cx,
        );
    }

    /// Writes a temporary query to `queries/` and replaces it with the saved
    /// document.
    fn save_scratch(
        &mut self,
        scratch: ScratchId,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placement = self.current_placement(cx);
        let create = self.model.update(cx, move |model, cx| {
            model.update_workspace(cx, move |workspace| {
                create_unique(
                    workspace,
                    document_folder(DocumentKind::Sql),
                    "query",
                    DocumentKind::Sql,
                    placement,
                    &text,
                )
            })
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = create.await;

            this.update_in(cx, |this, window, cx| {
                let Some(document) = Self::report(result, window, cx) else {
                    return;
                };

                if let Some(view) = this.scratches.remove(&scratch) {
                    view.update(cx, |view, cx| view.shut_down(cx));
                }

                let manifest = Arc::clone(this.model.read(cx).manifest());
                let scratches = this.scratch_ids();
                this.tabs.retain_locations(
                    |location| location_exists(&manifest, &scratches, location),
                    Location::Document(document),
                );
                this.sync_all(window, cx);
                this.navigate(
                    Location::Document(document),
                    ViewState::default(),
                    false,
                    window,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    fn focused_content(&self) -> Option<&TabContent> {
        self.slots
            .get(&self.tabs.focused_tab())
            .map(|slot| &slot.content)
    }

    fn focused_location(&self) -> Location {
        self.tabs
            .tab(self.tabs.focused_tab())
            .map_or(Location::Root, |tab| tab.history.location().clone())
    }

    /// Stores each table's filters, sort, and selection in its history entry.
    fn capture_view(&mut self, id: TabId, cx: &App) {
        let Some(TabContent::Table(table)) = self.slots.get(&id).map(|slot| slot.content.clone())
        else {
            return;
        };

        let view = table.read(cx).view_state(cx);

        if let Some(tab) = self.tabs.tab_mut(id) {
            tab.history.update_view(view);
        }
    }

    fn capture_all_views(&mut self, cx: &App) {
        let ids: Vec<TabId> = self.slots.keys().copied().collect();

        for id in ids {
            self.capture_view(id, cx);
        }
    }

    // --- Navigation ------------------------------------------------------------

    pub fn navigate(
        &mut self,
        location: Location,
        view: ViewState,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.tabs.focused_tab();
        let replaces_content = !new_tab && location != self.focused_location();

        if !replaces_content {
            return self.navigate_now(location, view, new_tab, window, cx);
        }

        self.leave_tab(focused, window, cx, move |this, window, cx| {
            this.navigate_now(location, view, new_tab, window, cx)
        });
    }

    fn navigate_now(
        &mut self,
        location: Location,
        view: ViewState,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.tabs.focused_tab();
        self.capture_view(focused, cx);

        if new_tab {
            let id = self.tabs.open_tab(location);

            if let Some(tab) = self.tabs.tab_mut(id) {
                tab.history.update_view(view);
            }
        } else {
            self.tabs
                .focused_history_mut()
                .visit_with_view(location, view);
        }

        self.after_navigation(window, cx);
    }

    fn after_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.tabs.focused_tab();
        self.sync_slot(focused, window, cx);

        if let Some(content) = self.focused_content() {
            content.clone().focus(window, cx);
        }

        self.schedule_ui_save(cx);
        cx.notify();
    }

    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.tabs.focused_tab();
        self.leave_tab(focused, window, cx, Self::go_back_now);
    }

    fn go_back_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.tabs.focused_tab();
        self.capture_view(focused, cx);

        if self.tabs.focused_history_mut().go_back().is_some() {
            self.slots.remove(&focused);
            self.after_navigation(window, cx);
        }
    }

    fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.tabs.focused_tab();
        self.leave_tab(focused, window, cx, Self::go_forward_now);
    }

    fn go_forward_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.tabs.focused_tab();
        self.capture_view(focused, cx);

        if self.tabs.focused_history_mut().go_forward().is_some() {
            self.slots.remove(&focused);
            self.after_navigation(window, cx);
        }
    }

    fn activate_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.tabs.focused_tab();

        if focused == id {
            return self.activate_tab_now(id, window, cx);
        }

        self.leave_tab(focused, window, cx, move |this, window, cx| {
            this.activate_tab_now(id, window, cx)
        });
    }

    fn activate_tab_now(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.activate(id);
        self.after_navigation(window, cx);
    }

    fn cycle_tab(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = self.tabs.tabs();
        let Some(position) = tabs
            .iter()
            .position(|tab| tab.id == self.tabs.focused_tab())
        else {
            return;
        };

        let next = (position as isize + step).rem_euclid(tabs.len() as isize) as usize;
        let id = tabs[next].id;
        self.activate_tab(id, window, cx);
    }

    fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_tab(id, window, cx, move |this, window, cx| {
            this.close_tab_now(id, window, cx)
        });
    }

    fn close_tab_now(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let needs_confirmation = matches!(
            self.slots.get(&id).map(|slot| &slot.content),
            Some(TabContent::Document(document)) if document.read(cx).needs_confirmation_to_close(cx)
        );

        if needs_confirmation {
            window.push_notification(
                Notification::warning("Commit or roll back the open transaction, or wait for the statement, before closing."),
                cx,
            );
            return;
        }

        let closed = self.slots.get(&id).map(|slot| slot.content.location(cx));
        self.tabs.close(id, Location::Root);
        self.slots.remove(&id);

        // A temporary query ends with its last tab.
        if let Some(Location::Scratch(scratch)) = closed {
            let still_open = self
                .tabs
                .tabs()
                .iter()
                .any(|tab| *tab.history.location() == Location::Scratch(scratch));

            if !still_open && let Some(view) = self.scratches.remove(&scratch) {
                view.update(cx, |view, cx| view.shut_down(cx));
            }
        }
        self.sync_all(window, cx);
        self.after_navigation(window, cx);
    }

    fn toggle_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.split().is_some() {
            self.tabs.unsplit();
        } else {
            let location = self.focused_location();
            let other = self.tabs.open_tab(location);
            self.tabs
                .activate(self.tabs.tabs().first().map_or(other, |tab| tab.id));
            let primary = self.tabs.active();
            let secondary = if primary == other {
                self.tabs.tabs()[0].id
            } else {
                other
            };
            self.tabs.split_with(secondary, SplitAxis::Horizontal);
        }

        self.sync_all(window, cx);
        self.after_navigation(window, cx);
    }

    fn focus_other_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = match self.tabs.focused_pane() {
            Pane::Primary => Pane::Secondary,
            Pane::Secondary => Pane::Primary,
        };

        self.tabs.focus(pane);
        self.after_navigation(window, cx);
    }

    // --- Events ----------------------------------------------------------------

    fn on_workspace_event(
        &mut self,
        event: &WorkspaceEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorkspaceEvent::DocumentChanged(document)
            | WorkspaceEvent::DocumentRemoved(document) => {
                if let Some(view) = self.documents.get(document).cloned() {
                    view.update(cx, |view, cx| view.file_changed(window, cx));
                }
            }
            WorkspaceEvent::ManifestChanged => {
                let manifest = Arc::clone(self.model.read(cx).manifest());
                let scratches = self.scratch_ids();
                self.documents
                    .retain(|document, _| manifest.document(*document).is_some());
                self.tabs.retain_locations(
                    |location| location_exists(&manifest, &scratches, location),
                    Location::Root,
                );
                self.sync_all(window, cx);
                window.set_window_title(&self.title(cx));
            }
            WorkspaceEvent::ConnectionChanged(_) => {}
            WorkspaceEvent::SaveFailed(error) => window.push_notification(
                Notification::error(format!("Workspace autosave failed: {error}")),
                cx,
            ),
        }

        cx.notify();
    }

    fn on_content_event(
        &mut self,
        event: ContentEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ContentEvent::Navigate {
                location,
                view,
                new_tab,
            } => self.navigate(location, view, new_tab, window, cx),
            ContentEvent::OpenUntracked(path) => self.open_untracked(path, window, cx),
            ContentEvent::SetUpConnection(connection) => {
                connection_dialog::open(self.model.clone(), Some(connection), window, cx);
            }
            ContentEvent::NewDocument { kind, object } => {
                self.new_document(kind, object, window, cx)
            }
            ContentEvent::NewFolder => self.new_folder(window, cx),
            ContentEvent::RenameDocument(document) => self.rename_document(document, window, cx),
            ContentEvent::RemoveDocument {
                document,
                delete_file,
            } => self.remove_document(document, delete_file, window, cx),
            ContentEvent::AddFavorite(location) => {
                if !self.favorites.contains(&location) {
                    self.favorites.push(location);
                    self.schedule_ui_save(cx);
                }
            }
            ContentEvent::MoveEntry { entry, placement } => {
                let task = self.model.update(cx, move |model, cx| {
                    model.update_workspace(cx, move |workspace| {
                        workspace.move_entry(entry, placement)
                    })
                });
                self.spawn_report(task, window, cx);
            }
            ContentEvent::Associate { document, object } => {
                let task = self.model.update(cx, move |model, cx| {
                    model.update_workspace(cx, move |workspace| {
                        workspace.associate(document, object)
                    })
                });
                self.spawn_report(task, window, cx);
            }
            ContentEvent::Dissociate { document, object } => {
                let task = self.model.update(cx, move |model, cx| {
                    model.update_workspace(cx, move |workspace| {
                        workspace.dissociate(document, &object)
                    })
                });
                self.spawn_report(task, window, cx);
            }
            ContentEvent::SaveScratch { scratch, text } => {
                self.save_scratch(scratch, text, window, cx)
            }
            ContentEvent::Changed => {}
        }

        cx.notify();
    }

    fn report<T>(result: Result<T, SharedString>, window: &mut Window, cx: &mut App) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                window.push_notification(Notification::error(error), cx);
                None
            }
        }
    }

    fn open_untracked(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let register = self.model.update(cx, |model, cx| {
            model.update_workspace(cx, move |workspace| {
                workspace.register_document(
                    &qwerio_workspace::paths::from_portable(&path),
                    Placement::Root,
                )
            })
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = register.await;

            this.update_in(cx, |this, window, cx| {
                if let Some(document) = Self::report(result, window, cx) {
                    this.navigate(
                        Location::Document(document),
                        ViewState::default(),
                        false,
                        window,
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    /// The placement for new entries: the current container, or beside the
    /// current table or document.
    fn current_placement(&self, cx: &App) -> Placement {
        let manifest = self.model.read(cx).manifest();
        enclosing_container(manifest, &self.focused_location())
            .placement()
            .unwrap_or_default()
    }

    fn new_document(
        &mut self,
        kind: DocumentKind,
        object: Option<ObjectRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placement = match &object {
            Some(object) => Placement::container(object.connection, &object.container()),
            None => self.current_placement(cx),
        };
        let stem = match &object {
            Some(object) => templates::file_stem(&object.name),
            None if kind == DocumentKind::Sql => "query".to_owned(),
            None => "note".to_owned(),
        };
        let text = match (&object, kind) {
            (Some(object), DocumentKind::Sql) => templates::select_statement(object),
            (Some(object), _) => templates::note(object),
            (None, _) => String::new(),
        };
        let folder = document_folder(kind);

        let create = self.model.update(cx, move |model, cx| {
            model.update_workspace(cx, move |workspace| {
                let document = create_unique(workspace, folder, &stem, kind, placement, &text)?;

                if let Some(object) = object {
                    if kind == DocumentKind::Sql {
                        workspace.set_execution_binding(
                            document,
                            Some(ExecutionBinding {
                                connection: object.connection,
                                database: object.database.clone(),
                            }),
                        )?;
                    }

                    workspace.associate(document, object)?;
                }

                Ok(document)
            })
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = create.await;

            this.update_in(cx, |this, window, cx| {
                if let Some(document) = Self::report(result, window, cx) {
                    this.navigate(
                        Location::Document(document),
                        ViewState::default(),
                        false,
                        window,
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    /// Asks for a name in a dialog and passes it to `apply`.
    fn prompt_name(
        title: &'static str,
        initial: String,
        action: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        let this = cx.entity().downgrade();
        let apply = std::rc::Rc::new(apply);
        let focus = std::cell::Cell::new(true);

        // Returns whether a name was given; the dialog stays open otherwise.
        let submit = {
            let input = input.clone();
            std::rc::Rc::new(move |window: &mut Window, cx: &mut App| -> bool {
                let name = input.read(cx).value().trim().to_owned();

                if name.is_empty() {
                    return false;
                }

                let apply = std::rc::Rc::clone(&apply);
                this.update(cx, |this, cx| apply(this, name, window, cx))
                    .ok();
                true
            })
        };

        window.open_dialog(cx, move |dialog, window, cx| {
            if focus.replace(false) {
                let handle = input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }

            let on_ok = std::rc::Rc::clone(&submit);
            let on_click = std::rc::Rc::clone(&submit);

            dialog
                .title(title)
                .w(px(420.))
                .child(Input::new(&input))
                .on_ok(move |_, window, cx| on_ok(window, cx))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            DialogClose::new()
                                .child(Button::new("cancel").outline().label("Cancel")),
                        )
                        .child(Button::new("confirm").primary().label(action).on_click(
                            move |_, window, cx| {
                                if on_click(window, cx) {
                                    window.close_dialog(cx);
                                }
                            },
                        )),
                )
        });
    }

    fn new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placement = self.current_placement(cx);

        Self::prompt_name(
            "New Folder",
            String::new(),
            "Create",
            window,
            cx,
            move |this, name, window, cx| {
                let placement = placement.clone();
                let create = this.model.update(cx, move |model, cx| {
                    model.update_workspace(cx, move |workspace| {
                        workspace.create_folder(&name, placement)
                    })
                });
                this.spawn_report(create, window, cx);
            },
        );
    }

    fn rename_document(
        &mut self,
        document: DocumentId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .model
            .read(cx)
            .manifest()
            .document(document)
            .map(|entry| entry.file_name().to_owned())
            .unwrap_or_default();

        Self::prompt_name(
            "Rename Document",
            current,
            "Rename",
            window,
            cx,
            move |this, name, window, cx| {
                let rename = this.model.update(cx, move |model, cx| {
                    model.update_workspace(cx, move |workspace| {
                        workspace.rename_document(document, &name)
                    })
                });
                this.spawn_report(rename, window, cx);
            },
        );
    }

    fn remove_document(
        &mut self,
        document: DocumentId,
        delete_file: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self
            .model
            .read(cx)
            .manifest()
            .document(document)
            .map(|entry| entry.file_name().to_owned())
            .unwrap_or_default();
        let this = cx.entity().downgrade();
        let (title, description, ok) = if delete_file {
            (
                format!("Delete “{name}”?"),
                "The file is deleted from disk.",
                "Delete",
            )
        } else {
            (
                format!("Remove “{name}” from the workspace?"),
                "The file stays on disk and appears again as a file that is not in the workspace.",
                "Remove",
            )
        };

        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            let alert = alert
                .title(title.clone())
                .description(description)
                .ok_text(ok)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| {
                        if let Some(view) = this.documents.remove(&document) {
                            view.update(cx, |view, cx| view.shut_down(cx));
                        }

                        let remove = this.model.update(cx, move |model, cx| {
                            model.update_workspace(cx, move |workspace| {
                                workspace.remove_document(document, delete_file)
                            })
                        });
                        this.spawn_report(remove, window, cx);
                    })
                    .ok();
                    true
                });

            if delete_file {
                alert.ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
            } else {
                alert
            }
        });
    }

    fn spawn_report<T: 'static>(
        &mut self,
        task: Task<Result<T, SharedString>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |_, cx| {
            let result = task.await;
            cx.update(|window, cx| {
                Self::report(result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn apply_entry_filter(&mut self, cx: &mut Context<Self>) {
        let filter = self.entry_filter.read(cx).value().to_string();

        for slot in self.slots.values() {
            if let TabContent::Explorer(explorer) = &slot.content {
                explorer.update(cx, |explorer, cx| explorer.set_filter(&filter, cx));
            }
        }
    }

    fn set_layout(&mut self, layout: ExplorerLayout, cx: &mut Context<Self>) {
        self.explorer_layout = layout;

        for slot in self.slots.values() {
            if let TabContent::Explorer(explorer) = &slot.content {
                explorer.update(cx, |explorer, cx| explorer.set_layout(layout, cx));
            }
        }

        self.schedule_ui_save(cx);
        cx.notify();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.focused_content().cloned() {
            Some(TabContent::Explorer(view)) => view.update(cx, |view, cx| view.refresh(cx)),
            Some(TabContent::Table(view)) => view.update(cx, |view, cx| view.refresh(window, cx)),
            Some(TabContent::Document(view)) => {
                view.update(cx, |view, cx| view.file_changed(window, cx))
            }
            None => {}
        }
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if is_narrow(window) {
            self.sidebar_open_when_narrow = !self.sidebar_open_when_narrow;
        } else {
            self.sidebar_open = !self.sidebar_open;
            self.schedule_ui_save(cx);
        }

        cx.notify();
    }

    fn toggle_inspector(&mut self, cx: &mut Context<Self>) {
        self.inspector_open = !self.inspector_open;

        if let Some(TabContent::Table(table)) = self.focused_content().cloned() {
            let open = self.inspector_open;
            table.update(cx, |table, cx| table.set_inspector_open(open, cx));
        }

        self.schedule_ui_save(cx);
        cx.notify();
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.palette.clone();
        let commands = palette_commands();
        let focus = std::cell::Cell::new(true);

        window.open_dialog(cx, move |dialog, window, cx| {
            if focus.replace(false) {
                let handle = state.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }

            let items: Vec<CommandItem> = commands
                .iter()
                .map(|(label, _)| CommandItem::new().label(*label))
                .collect();
            let actions: Vec<Box<dyn Action>> = commands
                .iter()
                .map(|(_, action)| action.boxed_clone())
                .collect();
            let state = state.clone();

            dialog
                .close_button(false)
                .p_0()
                .w(px(460.))
                .content(move |content, _, _| {
                    let actions: Vec<Box<dyn Action>> =
                        actions.iter().map(|action| action.boxed_clone()).collect();

                    content.child(
                        Command::new(&state)
                            .bordered(false)
                            .placeholder("Type a command")
                            .items(items.clone())
                            .on_confirm(move |index, window, cx| {
                                let Some(action) =
                                    actions.get(index.row).map(|action| action.boxed_clone())
                                else {
                                    return;
                                };

                                window.close_dialog(cx);
                                window.defer(cx, move |window, cx| {
                                    window.dispatch_action(action, cx)
                                });
                            }),
                    )
                })
        });
    }

    // --- Rendering -------------------------------------------------------------

    fn is_sidebar_visible(&self, window: &Window) -> bool {
        if is_narrow(window) {
            self.sidebar_open_when_narrow
        } else {
            self.sidebar_open
        }
    }

    fn render_place(
        &self,
        id: SharedString,
        icon: Icon,
        label: SharedString,
        detail: Option<SharedString>,
        target: Location,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_current = self.focused_location() == target;

        Button::new(id)
            .ghost()
            .small()
            .w_full()
            .justify_start()
            .selected(is_current)
            .icon(icon)
            .accessibility_label(label.clone())
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .when_some(detail, |this, detail| {
                        this.child(div().text_xs().text_color(cx.theme().warning).child(detail))
                    }),
            )
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    this.navigate(
                        target.clone(),
                        ViewState::default(),
                        event.modifiers().secondary(),
                        window,
                        cx,
                    );
                }),
            )
            .into_any_element()
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let manifest = Arc::clone(model.manifest());

        let connections: Vec<_> = manifest
            .connections
            .iter()
            .map(|connection| {
                let detail =
                    (!model.is_bound(connection.id)).then(|| SharedString::from("Not set up"));
                (
                    connection.id,
                    connection.name.clone(),
                    connection.engine,
                    detail,
                )
            })
            .collect();

        let favorites: Vec<(Location, SharedString)> = self
            .favorites
            .iter()
            .map(|location| {
                let label = breadcrumbs(&manifest, location)
                    .last()
                    .map(|crumb| crumb.label.clone())
                    .unwrap_or_default();
                (location.clone(), label.into())
            })
            .collect();

        let mut favorite_places = Vec::with_capacity(favorites.len());

        for (index, (location, label)) in favorites.into_iter().enumerate() {
            let place = self.render_place(
                format!("favorite-{index}").into(),
                Icon::new(IconName::Star),
                label,
                None,
                location.clone(),
                cx,
            );
            let this = cx.entity().downgrade();

            favorite_places.push(
                div()
                    .id(SharedString::from(format!("favorite-menu-{index}")))
                    .child(place)
                    .context_menu(move |menu, _, _| {
                        let this = this.clone();
                        let location = location.clone();
                        menu.item(PopupMenuItem::new("Remove from Favorites").on_click(
                            move |_, _, cx| {
                                this.update(cx, |this, cx| {
                                    this.favorites.retain(|favorite| *favorite != location);
                                    this.schedule_ui_save(cx);
                                    cx.notify();
                                })
                                .ok();
                            },
                        ))
                    })
                    .into_any_element(),
            );
        }

        let mut connection_places = Vec::with_capacity(connections.len());

        for (id, name, engine, detail) in connections {
            let place = self.render_place(
                format!("connection-{id}").into(),
                entry_icon(
                    qwerio_workspace::explorer::EntryKind::Connection(engine),
                    cx,
                ),
                name.into(),
                detail,
                Location::connection_root(id),
                cx,
            );
            let model = self.model.clone();
            connection_places.push(
                div()
                    .id(SharedString::from(format!("connection-menu-{id}")))
                    .child(place)
                    .context_menu(move |menu, _, _| {
                        let model = model.clone();
                        menu.item(PopupMenuItem::new("Edit Connection…").on_click(
                            move |_, window, cx| {
                                connection_dialog::open(model.clone(), Some(id), window, cx);
                            },
                        ))
                    })
                    .into_any_element(),
            );
        }

        let workspace_switcher = div()
            .id("workspace-name")
            .px_2()
            .py_1()
            .font_semibold()
            .child(manifest.name.clone());

        let root_place = self.render_place(
            "place-root".into(),
            Icon::new(IconName::FolderOpen),
            manifest.name.clone().into(),
            None,
            Location::Root,
            cx,
        );

        let filters: Option<AnyElement> = match self.focused_content() {
            Some(TabContent::Explorer(_)) => Some(
                Input::new(&self.entry_filter)
                    .small()
                    .cleanable(true)
                    .into_any_element(),
            ),
            Some(TabContent::Table(table)) => {
                Some(table.read(cx).filter_panel().clone().into_any_element())
            }
            _ => None,
        };
        let filter_title = match self.focused_content() {
            Some(TabContent::Table(_)) => "Filter records",
            _ => "Filter items",
        };

        v_flex()
            .id("sidebar")
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .overflow_y_scroll()
            .p_2()
            .gap_3()
            .child(workspace_switcher)
            .child(
                v_flex()
                    .gap_0p5()
                    .child(root_place)
                    .when(!favorite_places.is_empty(), |this| {
                        this.child(sidebar_heading("Favorites", cx))
                            .children(favorite_places)
                    })
                    .child(sidebar_heading("Connections", cx))
                    .children(connection_places)
                    .child(
                        Button::new("add-connection")
                            .ghost()
                            .small()
                            .w_full()
                            .justify_start()
                            .icon(IconName::Plus)
                            .label("New Connection…")
                            .on_click(cx.listener(|this, _, window, cx| {
                                connection_dialog::open(this.model.clone(), None, window, cx);
                            })),
                    ),
            )
            .when_some(filters, |this, filters| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(sidebar_heading(filter_title, cx))
                        .child(filters),
                )
            })
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs: Vec<(TabId, SharedString)> = self
            .tabs
            .tabs()
            .iter()
            .map(|tab| {
                let title = self
                    .slots
                    .get(&tab.id)
                    .map(|slot| slot.content.title(cx))
                    .unwrap_or_default();
                (tab.id, title)
            })
            .collect();
        let ids: Vec<TabId> = tabs.iter().map(|(id, _)| *id).collect();
        let selected = ids
            .iter()
            .position(|id| *id == self.tabs.active())
            .unwrap_or_default();

        TabBar::new("tabs")
            .small()
            .selected_index(selected)
            .on_click(cx.listener(move |this, index: &usize, window, cx| {
                if let Some(id) = ids.get(*index) {
                    this.tabs.focus(Pane::Primary);
                    this.activate_tab(*id, window, cx);
                }
            }))
            .children(tabs.into_iter().map(|(id, title)| {
                Tab::new().label(title).suffix(
                    Button::new(SharedString::from(format!("close-tab-{}", id.get())))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Close tab")
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.close_tab(id, window, cx)),
                        ),
                )
            }))
            .suffix(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("new-tab")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip("New tab")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let location = this.focused_location();
                                this.navigate(location, ViewState::default(), true, window, cx);
                            })),
                    )
                    .child(
                        Button::new("split")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(gpui_kit::assets::IconName::Split))
                            .tooltip("Split")
                            .toggled(self.tabs.split().is_some())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.toggle_split(window, cx)),
                            ),
                    ),
            )
    }

    fn render_pane(&self, tab: TabId, pane: Pane, cx: &mut Context<Self>) -> AnyElement {
        let is_focused = self.tabs.focused_pane() == pane;
        let content = self.slots.get(&tab).map(|slot| slot.content.element());

        div()
            .id(SharedString::from(format!("pane-{}", tab.get())))
            .size_full()
            .min_w_0()
            .min_h_0()
            .when(self.tabs.split().is_some() && is_focused, |this| {
                this.border_t_2().border_color(cx.theme().ring)
            })
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if this.tabs.focused_pane() != pane {
                        this.tabs.focus(pane);
                        cx.notify();
                    }
                }),
            )
            .children(content)
            .into_any_element()
    }

    fn render_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let primary = self.render_pane(self.tabs.active(), Pane::Primary, cx);

        match self.tabs.split() {
            None => primary,
            Some(split) => {
                let secondary_pane = self.render_pane(split.tab, Pane::Secondary, cx);
                let title = self
                    .slots
                    .get(&split.tab)
                    .map(|slot| slot.content.title(cx))
                    .unwrap_or_default();
                let secondary = v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .px_2()
                            .h_8()
                            .gap_2()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().tab_bar)
                            .child(div().flex_1().truncate().text_sm().child(title))
                            .child(
                                Button::new("close-split")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .tooltip("Close split")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_split(window, cx)
                                    })),
                            ),
                    )
                    .child(div().flex_1().min_h_0().child(secondary_pane));

                h_resizable("content-split")
                    .child(resizable_panel().child(primary))
                    .child(resizable_panel().child(secondary))
                    .into_any_element()
            }
        }
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self
            .focused_content()
            .map(|content| content.status(cx))
            .unwrap_or_default();
        let root = self.model.read(cx).root().display().to_string();

        h_flex()
            .h_7()
            .flex_shrink_0()
            .px_3()
            .gap_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(
                div()
                    .id("status")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .child(status)
                    .test_support(),
            )
            .child(div().max_w_1_3().truncate().child(root))
    }
}

/// A command shown in the palette and the action it dispatches.
fn palette_commands() -> Vec<(&'static str, Box<dyn Action>)> {
    vec![
        ("New Temporary Query", Box::new(NewTemporaryQuery)),
        ("New SQL Document", Box::new(NewSqlDocument)),
        ("New Note", Box::new(NewNote)),
        ("New Folder", Box::new(NewFolder)),
        ("New Connection…", Box::new(NewConnection)),
        ("New Database…", Box::new(NewDatabase)),
        ("New Table…", Box::new(NewTable)),
        ("New Column…", Box::new(NewColumn)),
        ("Go Back", Box::new(GoBack)),
        ("Go Forward", Box::new(GoForward)),
        ("Go to Workspace", Box::new(GoToWorkspaceRoot)),
        ("New Tab", Box::new(NewTab)),
        ("Close Tab", Box::new(CloseTab)),
        ("Split", Box::new(ToggleSplit)),
        ("Toggle Sidebar", Box::new(ToggleSidebar)),
        ("Toggle Inspector", Box::new(ToggleInspector)),
        ("Toggle List or Icons", Box::new(ToggleExplorerLayout)),
        ("Show Relationship Graph", Box::new(ShowRelationshipGraph)),
        ("Add to Favorites", Box::new(AddToFavorites)),
        ("Refresh", Box::new(Refresh)),
        ("Save", Box::new(Save)),
        ("Run Statement", Box::new(RunStatement)),
        ("Cancel Statement", Box::new(CancelStatement)),
        ("Begin Transaction", Box::new(BeginTransaction)),
        ("Commit", Box::new(CommitTransaction)),
        ("Roll Back", Box::new(RollbackTransaction)),
        ("Markdown Preview", Box::new(TogglePreview)),
        ("Bigger Text", Box::new(IncreaseFontSize)),
        ("Smaller Text", Box::new(DecreaseFontSize)),
        ("Actual Text Size", Box::new(ResetFontSize)),
        ("Open Workspace…", Box::new(OpenWorkspace)),
        ("New Workspace…", Box::new(NewWorkspace)),
        ("Import Workspace…", Box::new(ImportWorkspace)),
        ("Export Workspace…", Box::new(ExportWorkspace)),
        ("Close Workspace", Box::new(CloseWorkspace)),
        ("Settings…", Box::new(OpenSettings)),
        ("About Qwerio", Box::new(About)),
    ]
}

fn sidebar_heading(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .pt_2()
        .px_2()
        .text_xs()
        .font_semibold()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn is_narrow(window: &Window) -> bool {
    f32::from(window.viewport_size().width) < NARROW_WIDTH
}

fn location_exists(
    manifest: &qwerio_workspace::manifest::Manifest,
    scratches: &HashSet<ScratchId>,
    location: &Location,
) -> bool {
    match location {
        Location::Scratch(scratch) => scratches.contains(scratch),
        Location::Root | Location::Object(_) => true,
        Location::Folder(folder) => manifest.folder(*folder).is_some(),
        Location::Container { connection, .. } => manifest.connection(*connection).is_some(),
        Location::Document(document) => manifest.document(*document).is_some(),
    }
}

/// Creates `<folder>/<stem>.<ext>`, adding `-2`, `-3`, … when taken.
fn create_unique(
    workspace: &mut Workspace,
    folder: &str,
    stem: &str,
    kind: DocumentKind,
    placement: Placement,
    text: &str,
) -> Result<DocumentId, WorkspaceError> {
    const MAX_ATTEMPTS: usize = 1000;

    for attempt in 1..=MAX_ATTEMPTS {
        let name = if attempt == 1 {
            format!("{stem}.{}", kind.extension())
        } else {
            format!("{stem}-{attempt}.{}", kind.extension())
        };
        let path: PathBuf = [folder, &name].iter().collect();

        match workspace.create_document(&path, placement.clone(), text) {
            Ok((document, _)) => return Ok(document),
            Err(WorkspaceError::AlreadyExists(_)) => continue,
            Err(error) => return Err(error),
        }
    }

    Err(WorkspaceError::InvalidName(stem.to_owned()))
}

impl Focusable for WorkspaceShell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WorkspaceShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show_sidebar = self.is_sidebar_visible(window);
        let content = v_flex()
            .size_full()
            .min_w_0()
            .child(self.render_tab_bar(cx))
            .child(div().flex_1().min_h_0().child(self.render_content(cx)));

        let body = if show_sidebar {
            h_resizable("shell-sidebar")
                .child(
                    resizable_panel()
                        .size(px(SIDEBAR_WIDTH))
                        .size_range(px(SIDEBAR_MIN_WIDTH)..px(SIDEBAR_MAX_WIDTH))
                        .child(self.render_sidebar(cx)),
                )
                .child(resizable_panel().child(content))
                .into_any_element()
        } else {
            content.into_any_element()
        };

        v_flex()
            .id("workspace-shell")
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(|this, _: &GoBack, window, cx| this.go_back(window, cx)))
            .on_action(cx.listener(|this, _: &GoForward, window, cx| this.go_forward(window, cx)))
            .on_action(cx.listener(|this, _: &GoToWorkspaceRoot, window, cx| {
                this.navigate(Location::Root, ViewState::default(), false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewTab, window, cx| {
                let location = this.focused_location();
                this.navigate(location, ViewState::default(), true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                let id = this.tabs.focused_tab();
                this.close_tab(id, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(1, window, cx)))
            .on_action(
                cx.listener(|this, _: &PreviousTab, window, cx| this.cycle_tab(-1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleSplit, window, cx| this.toggle_split(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FocusOtherPane, window, cx| {
                    this.focus_other_pane(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ToggleSidebar, window, cx| this.toggle_sidebar(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleInspector, _, cx| this.toggle_inspector(cx)))
            .on_action(cx.listener(|this, _: &FocusExplorerFilter, window, cx| {
                if !this.is_sidebar_visible(window) {
                    this.toggle_sidebar(window, cx);
                }
                this.entry_filter
                    .update(cx, |input, cx| input.focus(window, cx));
            }))
            .on_action(
                cx.listener(|this, _: &OpenCommandPalette, window, cx| {
                    this.open_palette(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &Refresh, window, cx| this.refresh(window, cx)))
            .on_action(cx.listener(|this, _: &Save, window, cx| {
                if let Some(TabContent::Document(view)) = this.focused_content().cloned() {
                    view.update(cx, |view, cx| view.save(None, window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &NewConnection, window, cx| {
                connection_dialog::open(this.model.clone(), None, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &NewDatabase, window, cx| this.new_database(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NewTable, window, cx| this.new_table(window, cx)))
            .on_action(cx.listener(|this, _: &NewColumn, window, cx| this.new_column(window, cx)))
            .on_action(cx.listener(|this, _: &NewSqlDocument, window, cx| {
                this.new_document(DocumentKind::Sql, None, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewTemporaryQuery, window, cx| {
                this.new_temporary_query(window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewNote, window, cx| {
                this.new_document(DocumentKind::Markdown, None, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewFolder, window, cx| this.new_folder(window, cx)))
            .on_action(cx.listener(|this, _: &AddToFavorites, _, cx| {
                let location = this.focused_location();
                if !this.favorites.contains(&location) {
                    this.favorites.push(location);
                    this.schedule_ui_save(cx);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleExplorerLayout, _, cx| {
                let layout = match this.explorer_layout {
                    ExplorerLayout::Details => ExplorerLayout::Icons,
                    ExplorerLayout::Icons | ExplorerLayout::Graph => ExplorerLayout::Details,
                };
                this.set_layout(layout, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowRelationshipGraph, _, cx| {
                this.set_layout(ExplorerLayout::Graph, cx);
            }))
            .child(self.render_toolbar(window, cx))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_status_bar(cx))
    }
}
