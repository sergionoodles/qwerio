//! The file-manager view of a container: the workspace root, a folder, a
//! connection, a database, or a schema. Tables and documents share one list;
//! double-click or Enter opens the selection. Containers of tables and views
//! can also show the relationships between them as a graph.

mod graph;

#[cfg(test)]
pub(crate) use graph::node_widths;
mod icon_label;

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyContext, ParentElement as _, Render, Role,
    ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    TestSupportExt as _, UniformListScrollHandle, WeakEntity, Window, div, uniform_list,
};
use std::sync::Arc;

use qwerio_app::navigation::ViewState;
use qwerio_workspace::MovableEntry;
use qwerio_workspace::document::DocumentKind;
use qwerio_workspace::explorer::{
    EntryKind, EntryStatus, EntryTarget, ExplorerEntry, Location, filter_entries,
};
use qwerio_workspace::manifest::{DocumentId, Placement};
use serde::{Deserialize, Serialize};

use crate::actions::AddToFavorites;
use crate::content::{ContentEvent, entry_icon};
use crate::workspace_model::{ListingStatus, WorkspaceModel};

gpui_kit::actions!(
    explorer,
    [
        SelectPrevious,
        SelectNext,
        SelectFirst,
        SelectLast,
        OpenSelected,
        OpenSelectedInNewTab,
        ZoomIn,
        ZoomOut,
        ResetZoom
    ]
);

pub const EXPLORER_CONTEXT: &str = "Explorer";

/// Added to the explorer's key context while it shows the graph, so zoom
/// keys win over the workbench's text size keys only there.
const GRAPH_CONTEXT: &str = "graph";
const NO_ACCESS_LABEL: &str = "No access";

pub fn bind_keys(cx: &mut App) {
    let context = Some(EXPLORER_CONTEXT);

    cx.bind_keys([
        gpui_kit::KeyBinding::new("up", SelectPrevious, context),
        gpui_kit::KeyBinding::new("down", SelectNext, context),
        gpui_kit::KeyBinding::new("home", SelectFirst, context),
        gpui_kit::KeyBinding::new("end", SelectLast, context),
        gpui_kit::KeyBinding::new("enter", OpenSelected, context),
        gpui_kit::KeyBinding::new("secondary-enter", OpenSelectedInNewTab, context),
    ]);

    let graph = Some("Explorer && graph");
    cx.bind_keys([
        gpui_kit::KeyBinding::new("secondary-=", ZoomIn, graph),
        gpui_kit::KeyBinding::new("secondary-+", ZoomIn, graph),
        gpui_kit::KeyBinding::new("secondary--", ZoomOut, graph),
        gpui_kit::KeyBinding::new("secondary-0", ResetZoom, graph),
    ]);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExplorerLayout {
    #[default]
    Details,
    Icons,
    /// Tables and views with lines for their foreign keys.
    Graph,
}

pub struct ExplorerView {
    model: Entity<WorkspaceModel>,
    location: Location,
    all_entries: Vec<ExplorerEntry>,
    entries: Vec<ExplorerEntry>,
    status: ListingStatus,
    filter: String,
    layout: ExplorerLayout,
    graph: graph::GraphStatus,
    graph_viewport: graph::GraphViewport,
    selected_key: Option<String>,
    context_entry: Option<ExplorerEntry>,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,
    _model_subscription: Subscription,
}

impl EventEmitter<ContentEvent> for ExplorerView {}

impl ExplorerView {
    pub fn new(
        model: Entity<WorkspaceModel>,
        location: Location,
        layout: ExplorerLayout,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&model, |this, _, cx| this.reload(cx));
        let mut view = Self {
            model,
            location,
            all_entries: Vec::new(),
            entries: Vec::new(),
            status: ListingStatus::Loading,
            filter: String::new(),
            layout,
            graph: graph::GraphStatus::default(),
            graph_viewport: graph::GraphViewport::default(),
            selected_key: None,
            context_entry: None,
            focus_handle: cx.focus_handle(),
            scroll_handle: UniformListScrollHandle::new(),
            _model_subscription: subscription,
        };

        view.load_catalog(false, cx);
        view.reload(cx);
        view
    }

    pub fn location(&self) -> &Location {
        &self.location
    }

    pub fn title(&self, cx: &App) -> SharedString {
        qwerio_workspace::explorer::breadcrumbs(self.model.read(cx).manifest(), &self.location)
            .last()
            .map(|crumb| SharedString::from(crumb.label.clone()))
            .unwrap_or_else(|| "Workspace".into())
    }

    pub fn status_text(&self) -> SharedString {
        if let Some(text) = self.graph_status_text() {
            return text;
        }

        match &self.status {
            ListingStatus::Loading => "Loading…".into(),
            ListingStatus::Failed(_) => "Could not load this list".into(),
            ListingStatus::Unbound(_) => "Connection not set up on this computer".into(),
            ListingStatus::Ready if self.entries.len() == self.all_entries.len() => {
                format!("{} items", self.entries.len()).into()
            }
            ListingStatus::Ready => {
                format!("{} of {} items", self.entries.len(), self.all_entries.len()).into()
            }
        }
    }

    fn load_catalog(&mut self, force: bool, cx: &mut Context<Self>) {
        if let Location::Container { connection, path } = self.location.clone() {
            self.model.update(cx, |model, cx| {
                model.ensure_catalog(connection, path, force, cx)
            });
        }
    }

    /// Reloads the listing from the database and the workspace folder.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load_catalog(true, cx);
        self.load_relationships(true, cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let (entries, status) = self.model.read(cx).entries(&self.location);
        self.all_entries = entries;
        self.status = status;
        self.apply_filter();
        self.load_relationships(false, cx);
        self.rebuild_graph(cx);
        cx.emit(ContentEvent::Changed);
        cx.notify();
    }

    /// Filters visible entries by name. This never filters table records.
    pub fn set_filter(&mut self, filter: &str, cx: &mut Context<Self>) {
        if self.filter == filter {
            return;
        }

        self.filter = filter.to_owned();
        self.apply_filter();
        self.rebuild_graph(cx);
        cx.emit(ContentEvent::Changed);
        cx.notify();
    }

    pub fn set_layout(&mut self, layout: ExplorerLayout, cx: &mut Context<Self>) {
        self.layout = layout;
        self.load_relationships(false, cx);
        self.rebuild_graph(cx);
        cx.emit(ContentEvent::Changed);
        cx.notify();
    }

    fn apply_filter(&mut self) {
        self.entries = filter_entries(&self.all_entries, &self.filter);
    }

    fn selected_index(&self) -> Option<usize> {
        let key = self.selected_key.as_ref()?;
        self.entries.iter().position(|entry| &entry.key == key)
    }

    fn select_index(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };

        self.selected_key = Some(entry.key.clone());
        self.scroll_handle
            .scroll_to_item(index, ScrollStrategy::Center);
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            return;
        }

        let last = self.entries.len() - 1;
        let next = match self.selected_index() {
            None => 0,
            Some(index) => index.saturating_add_signed(delta).min(last),
        };

        self.select_index(next, cx);
    }

    fn open_entry(&mut self, entry: &ExplorerEntry, new_tab: bool, cx: &mut Context<Self>) {
        match &entry.target {
            EntryTarget::Open(location) => {
                if let (
                    EntryStatus::Unbound,
                    EntryKind::Connection(_),
                    Location::Container { connection, .. },
                ) = (entry.status, entry.kind, location)
                {
                    cx.emit(ContentEvent::SetUpConnection(*connection));
                    return;
                }

                cx.emit(ContentEvent::Navigate {
                    location: location.clone(),
                    view: ViewState::default(),
                    new_tab,
                });
            }
            EntryTarget::Untracked(path) => cx.emit(ContentEvent::OpenUntracked(path.clone())),
        }
    }

    fn open_selected(&mut self, new_tab: bool, cx: &mut Context<Self>) {
        if let Some(entry) = self
            .selected_index()
            .and_then(|index| self.entries.get(index))
            .cloned()
        {
            self.open_entry(&entry, new_tab, cx);
        }
    }

    fn on_row_click(
        &mut self,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.select_index(index, cx);

        if event.click_count() >= 2
            && let Some(entry) = self.entries.get(index).cloned()
        {
            self.open_entry(&entry, event.modifiers().secondary(), cx);
        }
    }

    fn entry_menu(
        this: WeakEntity<Self>,
        model: Entity<WorkspaceModel>,
        entry: ExplorerEntry,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let item = |label: &'static str, event: ContentEvent| {
            let this = this.clone();
            PopupMenuItem::new(label).on_click(move |_, _, cx| {
                this.update(cx, |_, cx| cx.emit(event.clone())).ok();
            })
        };

        let open_target = entry.target.clone();
        let open_in_new_tab = {
            let this = this.clone();
            let entry = entry.clone();
            PopupMenuItem::new("Open in New Tab").on_click(move |_, _, cx| {
                this.update(cx, |this, cx| this.open_entry(&entry, true, cx))
                    .ok();
            })
        };

        let menu = menu
            .item({
                let this = this.clone();
                let entry = entry.clone();
                PopupMenuItem::new("Open").on_click(move |_, _, cx| {
                    this.update(cx, |this, cx| this.open_entry(&entry, false, cx))
                        .ok();
                })
            })
            .item(open_in_new_tab);

        let label_target = match &open_target {
            EntryTarget::Open(location) => {
                qwerio_workspace::labels::LabelTarget::from_location(location)
            }
            EntryTarget::Untracked(_) => None,
        };
        let menu = if let Some(target) = label_target {
            let labels_model = model.clone();
            menu.item(
                PopupMenuItem::new("Labels…").on_click(move |_, window, cx| {
                    crate::label_dialog::open(labels_model.clone(), target.clone(), window, cx);
                }),
            )
        } else {
            menu
        };
        let menu = match (&entry.kind, &open_target) {
            (EntryKind::Object(_), EntryTarget::Open(Location::Object(object))) => menu
                .separator()
                .item(item(
                    "New Query for Table",
                    ContentEvent::NewDocument {
                        kind: DocumentKind::Sql,
                        object: Some(object.clone()),
                    },
                ))
                .item(item(
                    "New Note for Table",
                    ContentEvent::NewDocument {
                        kind: DocumentKind::Markdown,
                        object: Some(object.clone()),
                    },
                )),
            (
                EntryKind::Connection(_),
                EntryTarget::Open(Location::Container { connection, .. }),
            ) => menu.separator().item(item(
                "Edit Connection…",
                ContentEvent::SetUpConnection(*connection),
            )),
            (EntryKind::Document(_), EntryTarget::Open(Location::Document(document))) => menu
                .separator()
                .item(item("Rename…", ContentEvent::RenameDocument(*document)))
                .when(!model.read(cx).is_file_workspace(), |menu| {
                    menu.item(item(
                        "Remove from Workspace",
                        ContentEvent::RemoveDocument {
                            document: *document,
                            delete_file: false,
                        },
                    ))
                })
                .item(item(
                    "Delete File",
                    ContentEvent::RemoveDocument {
                        document: *document,
                        delete_file: true,
                    },
                )),
            (_, EntryTarget::Untracked(path)) => menu.separator().item(item(
                "Add to Workspace",
                ContentEvent::OpenUntracked(path.clone()),
            )),
            _ => menu,
        };

        let menu = match &open_target {
            EntryTarget::Open(location)
                if crate::permissions_dialog::available(model.read(cx), location) =>
            {
                menu.separator().item(item(
                    "Users & permissions…",
                    ContentEvent::OpenPermissions(location.clone()),
                ))
            }
            _ => menu,
        };

        let manifest = Arc::clone(model.read(cx).manifest());

        let menu = match entry.movable {
            Some(movable) => {
                let mut targets: Vec<(String, Placement)> =
                    vec![("Workspace".to_owned(), Placement::Root)];
                targets.extend(
                    manifest
                        .folders
                        .iter()
                        .filter(|folder| movable != MovableEntry::Folder(folder.id))
                        .map(|folder| {
                            (folder.name.clone(), Placement::Folder { folder: folder.id })
                        }),
                );
                let this = this.clone();

                menu.separator()
                    .submenu("Move To", window, cx, move |submenu, _, _| {
                        targets.iter().fold(submenu, |submenu, (label, placement)| {
                            let this = this.clone();
                            let placement = placement.clone();
                            submenu.item(PopupMenuItem::new(label.clone()).on_click(
                                move |_, _, cx| {
                                    let placement = placement.clone();
                                    this.update(cx, |_, cx| {
                                        cx.emit(ContentEvent::MoveEntry {
                                            entry: movable,
                                            placement,
                                        })
                                    })
                                    .ok();
                                },
                            ))
                        })
                    })
            }
            None => menu,
        };

        let menu = match &open_target {
            EntryTarget::Open(Location::Object(object)) if !manifest.documents.is_empty() => {
                let documents: Vec<(DocumentId, String)> = manifest
                    .documents
                    .iter()
                    .filter(|document| {
                        !document
                            .associations
                            .iter()
                            .any(|associated| associated.same_location_and_name(object))
                    })
                    .map(|document| (document.id, document.path.clone()))
                    .collect();
                let this = this.clone();
                let object = object.clone();

                menu.submenu("Link to Document", window, cx, move |submenu, _, _| {
                    documents.iter().fold(submenu, |submenu, (document, path)| {
                        let this = this.clone();
                        let document = *document;
                        let object = object.clone();
                        submenu.item(PopupMenuItem::new(path.clone()).on_click(move |_, _, cx| {
                            let object = object.clone();
                            this.update(cx, |_, cx| {
                                cx.emit(ContentEvent::Associate { document, object })
                            })
                            .ok();
                        }))
                    })
                })
            }
            _ => menu,
        };

        match open_target {
            EntryTarget::Open(location) => menu.separator().item(item(
                "Add to Favorites",
                ContentEvent::AddFavorite(location),
            )),
            EntryTarget::Untracked(_) => menu,
        }
    }

    fn background_menu(this: WeakEntity<Self>, menu: PopupMenu) -> PopupMenu {
        let item = |label: &'static str, event: ContentEvent| {
            let this = this.clone();
            PopupMenuItem::new(label).on_click(move |_, _, cx| {
                this.update(cx, |_, cx| cx.emit(event.clone())).ok();
            })
        };

        let refresh = {
            let this = this.clone();
            PopupMenuItem::new("Refresh").on_click(move |_, _, cx| {
                this.update(cx, |this, cx| this.refresh(cx)).ok();
            })
        };

        menu.item(item(
            "New SQL Document",
            ContentEvent::NewDocument {
                kind: DocumentKind::Sql,
                object: None,
            },
        ))
        .item(item(
            "New Note",
            ContentEvent::NewDocument {
                kind: DocumentKind::Markdown,
                object: None,
            },
        ))
        .item(item("New Folder", ContentEvent::NewFolder))
        .separator()
        .item(refresh)
    }

    fn render_status_badge(entry: &ExplorerEntry, cx: &App) -> Option<AnyElement> {
        let (label, color) = match entry.status {
            EntryStatus::Ready => return None,
            EntryStatus::NoPrivilege => return Some(Self::render_no_access_badge(entry, cx)),
            EntryStatus::MissingFile => ("File missing", cx.theme().warning),
            EntryStatus::Untracked => ("Not in workspace", cx.theme().muted_foreground),
            EntryStatus::Unbound => ("Not set up", cx.theme().warning),
        };

        Some(
            div()
                .text_xs()
                .text_color(color)
                .child(label)
                .into_any_element(),
        )
    }

    fn render_no_access_badge(entry: &ExplorerEntry, cx: &App) -> AnyElement {
        div()
            .id(SharedString::from(format!("{}-no-access", entry.key)))
            .flex_shrink_0()
            .child(
                Icon::new(LucideIcon::Ban)
                    .xsmall()
                    .text_color(cx.theme().muted_foreground),
            )
            .tooltip(|window, cx| Tooltip::new(NO_ACCESS_LABEL).build(window, cx))
            .aria_label(NO_ACCESS_LABEL)
            .into_any_element()
    }

    fn render_entry_icon(entry: &ExplorerEntry, cx: &App) -> Icon {
        let icon = entry_icon(entry.kind, cx);
        if is_muted(entry) {
            return icon.text_color(cx.theme().muted_foreground);
        }
        icon
    }

    fn render_detail_row(
        &self,
        index: usize,
        entry: ExplorerEntry,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_muted = is_muted(&entry);
        h_flex()
            .id(SharedString::from(entry.key.clone()))
            .w_full()
            .h_8()
            .px_3()
            .gap_2()
            .rounded(cx.theme().radius)
            .when(is_selected, |row| {
                row.bg(cx.theme().list_active)
                    .text_color(cx.theme().foreground)
            })
            .when(!is_selected, |row| {
                row.hover(|style| style.bg(cx.theme().list_hover))
            })
            .when(is_muted, |row| row.text_color(cx.theme().muted_foreground))
            .child(Self::render_entry_icon(&entry, cx).small())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(entry.name.clone()),
            )
            .when_some(entry.detail.clone(), |row, detail| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .max_w_1_3()
                        .truncate()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail),
                )
            })
            .children(Self::render_status_badge(&entry, cx))
            .on_any_mouse_down(cx.listener(move |this, _, _, _| {
                this.context_entry = this.entries.get(index).cloned();
            }))
            .on_click(cx.listener(move |this, event, window, cx| {
                this.on_row_click(index, event, window, cx)
            }))
            .role(Role::ListItem)
            .aria_label(entry.name.clone())
            .aria_selected(is_selected)
            .test_support()
            .into_any_element()
    }

    fn render_icon_tile(
        &self,
        index: usize,
        entry: ExplorerEntry,
        is_selected: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .id(SharedString::from(entry.key.clone()))
            .w_32()
            .p_2()
            .gap_1()
            .items_center()
            .rounded(cx.theme().radius)
            .when(is_selected, |tile| tile.bg(cx.theme().list_active))
            .when(!is_selected, |tile| {
                tile.hover(|style| style.bg(cx.theme().list_hover))
            })
            .when(is_muted(&entry), |tile| {
                tile.text_color(cx.theme().muted_foreground)
            })
            .child(Self::render_entry_icon(&entry, cx).large())
            .child(icon_label::render(&entry.name, window))
            .children(Self::render_status_badge(&entry, cx))
            .on_any_mouse_down(cx.listener(move |this, _, _, _| {
                this.context_entry = this.entries.get(index).cloned();
            }))
            .on_click(cx.listener(move |this, event, window, cx| {
                this.on_row_click(index, event, window, cx)
            }))
            .role(Role::ListItem)
            .aria_label(entry.name.clone())
            .aria_selected(is_selected)
            .test_support()
            .into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, body, action): (SharedString, Option<SharedString>, Option<Button>) =
            match &self.status {
                ListingStatus::Loading => {
                    return v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .child(Spinner::new())
                        .into_any_element();
                }
                ListingStatus::Failed(error) => (
                    "Could not load this list".into(),
                    Some(error.clone()),
                    Some(
                        Button::new("retry")
                            .outline()
                            .label("Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
                ),
                ListingStatus::Unbound(connection) => {
                    let connection = *connection;
                    (
                    "This connection is not set up on this computer".into(),
                    Some("Its address or database file stays on each computer; the workspace only shares its name.".into()),
                    Some(
                        Button::new("set-up")
                            .primary()
                            .label("Set Up Connection…")
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(ContentEvent::SetUpConnection(connection)))),
                    ),
                )
                }
                ListingStatus::Ready if !self.filter.is_empty() => {
                    ("No items match the filter".into(), None, None)
                }
                ListingStatus::Ready => (
                    "Nothing here yet".into(),
                    Some("Create a query or note, or add a connection.".into()),
                    Some(
                        Button::new("new-document")
                            .outline()
                            .icon(IconName::Plus)
                            .label("New SQL Document")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ContentEvent::NewDocument {
                                    kind: DocumentKind::Sql,
                                    object: None,
                                })
                            })),
                    ),
                ),
            };

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_6()
            .child(div().font_medium().child(title))
            .when_some(body, |this, body| {
                this.child(
                    div()
                        .max_w_96()
                        .text_sm()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child(body),
                )
            })
            .children(action)
            .into_any_element()
    }
}

impl Focusable for ExplorerView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ExplorerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected_index();
        let this = cx.entity().downgrade();
        let model = self.model.clone();

        let body = if self.entries.is_empty() {
            self.render_empty(cx).into_any_element()
        } else {
            match self.effective_layout() {
                ExplorerLayout::Graph => self.render_graph(selected, cx),
                ExplorerLayout::Details => {
                    let view = cx.entity();
                    uniform_list("explorer-rows", self.entries.len(), move |range, _, cx| {
                        view.update(cx, |this, cx| {
                            range
                                .map(|index| {
                                    let entry = this.entries[index].clone();
                                    this.render_detail_row(
                                        index,
                                        entry,
                                        selected == Some(index),
                                        cx,
                                    )
                                })
                                .collect::<Vec<_>>()
                        })
                    })
                    .track_scroll(&self.scroll_handle)
                    .size_full()
                    .p_1()
                    .into_any_element()
                }
                ExplorerLayout::Icons => {
                    let entries = self.entries.clone();
                    let mut tiles = Vec::with_capacity(entries.len());

                    for (index, entry) in entries.into_iter().enumerate() {
                        tiles.push(self.render_icon_tile(
                            index,
                            entry,
                            selected == Some(index),
                            window,
                            cx,
                        ));
                    }

                    div()
                        .id("explorer-icons")
                        .size_full()
                        .overflow_y_scroll()
                        .p_2()
                        .child(h_flex().flex_wrap().items_start().gap_1().children(tiles))
                        .into_any_element()
                }
            }
        };

        let mut key_context = KeyContext::new_with_defaults();
        key_context.add(EXPLORER_CONTEXT);
        if self.effective_layout() == ExplorerLayout::Graph {
            key_context.add(GRAPH_CONTEXT);
        }

        div()
            .id("explorer")
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .size_full()
            .min_h_0()
            .test_support()
            // Clear before row handlers run, so empty space cannot reuse an entry menu.
            .capture_any_mouse_down(cx.listener(|this, _, _, _| {
                this.context_entry = None;
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select_index(0, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                let last = this.entries.len().saturating_sub(1);
                this.select_index(last, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| this.open_selected(false, cx)))
            .on_action(
                cx.listener(|this, _: &OpenSelectedInNewTab, _, cx| this.open_selected(true, cx)),
            )
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom_graph_in(cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom_graph_out(cx)))
            .on_action(cx.listener(|this, _: &ResetZoom, _, cx| this.reset_graph_zoom(cx)))
            .on_action(cx.listener(|this, _: &AddToFavorites, _, cx| {
                let location = this.location.clone();
                cx.emit(ContentEvent::AddFavorite(location));
            }))
            .child(body)
            .context_menu(move |menu, window, cx| {
                let entry = this
                    .upgrade()
                    .and_then(|view| view.read(cx).context_entry.clone());

                match entry {
                    Some(entry) => {
                        Self::entry_menu(this.clone(), model.clone(), entry, menu, window, cx)
                    }
                    None => Self::background_menu(this.clone(), menu),
                }
            })
    }
}

fn is_muted(entry: &ExplorerEntry) -> bool {
    matches!(
        entry.status,
        EntryStatus::NoPrivilege | EntryStatus::Untracked
    )
}
