//! The workspace toolbar: navigation and breadcrumbs, then commands grouped
//! the way file managers group them. Buttons that create database objects
//! are enabled only where the focused location allows them.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::breadcrumb::{Breadcrumb, BreadcrumbItem};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::separator::Separator;
use gpui_kit::component::toolbar::{Toolbar, ToolbarGroup};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, FocusHandle, IntoElement, OwnedMenuItem, ParentElement as _, Styled as _, Window, div,
};
use qwerio_app::navigation::ViewState;
use qwerio_core::resource::ObjectKind;
use qwerio_workspace::document::DocumentKind;
use qwerio_workspace::explorer::{EntryKind, breadcrumbs};

use super::{TabContent, WorkspaceShell};
use crate::about_dialog;
use crate::actions::OpenSettings;
use crate::connection_dialog;
use crate::content::{document_icon, entry_icon, object_icon};
use crate::explorer_view::ExplorerLayout;

impl WorkspaceShell {
    pub(super) fn render_toolbar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history = self
            .tabs
            .tab(self.tabs.focused_tab())
            .map(|tab| &tab.history);
        let can_go_back = history.is_some_and(|history| history.can_go_back());
        let can_go_forward = history.is_some_and(|history| history.can_go_forward());

        let navigation = ToolbarGroup::new("toolbar-navigation")
            .label("Navigation")
            // macOS provides the native menu bar; other platforms need an in-window menu.
            .when(!cfg!(target_os = "macos"), |navigation| {
                navigation.content(render_application_menu(
                    window
                        .focused(cx)
                        .unwrap_or_else(|| self.focus_handle.clone()),
                ))
            })
            .child(
                Button::new("toggle-sidebar")
                    .ghost()
                    .icon(IconName::PanelLeft)
                    .tooltip("Sidebar")
                    .toggled(self.is_sidebar_visible(window))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx))),
            )
            .child(
                Button::new("back")
                    .ghost()
                    .icon(IconName::ArrowLeft)
                    .tooltip("Back")
                    .disabled(!can_go_back)
                    .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx))),
            )
            .child(
                Button::new("forward")
                    .ghost()
                    .icon(IconName::ArrowRight)
                    .tooltip("Forward")
                    .disabled(!can_go_forward)
                    .on_click(cx.listener(|this, _, window, cx| this.go_forward(window, cx))),
            );

        let view = ToolbarGroup::new("toolbar-view")
            .label("View")
            .child(
                Button::new("refresh")
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .tooltip("Refresh")
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
            .children(self.render_layout_buttons(cx))
            .child(
                Button::new("palette")
                    .ghost()
                    .icon(IconName::Search)
                    .tooltip("Commands")
                    .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx))),
            );

        Toolbar::new("workspace-toolbar")
            .h_10()
            .px_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(navigation)
            .content(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .px_2()
                    .child(self.render_breadcrumb(cx)),
            )
            .child(self.render_schema_group(cx))
            .content(group_separator())
            .child(self.render_workspace_group(cx))
            .content(group_separator())
            .child(view)
            .content(group_separator())
            .child(render_app_group(self.focus_handle.clone()))
    }

    fn render_breadcrumb(&self, cx: &mut Context<Self>) -> Breadcrumb {
        let manifest = std::sync::Arc::clone(self.model.read(cx).manifest());
        let crumbs = breadcrumbs(&manifest, &self.focused_location());
        let last = crumbs.len().saturating_sub(1);

        crumbs
            .into_iter()
            .enumerate()
            .fold(Breadcrumb::new(), |breadcrumb, (index, crumb)| {
                if index == last {
                    return breadcrumb.child(BreadcrumbItem::new(crumb.label));
                }

                let target = crumb.location;
                breadcrumb.child(BreadcrumbItem::new(crumb.label).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.navigate(target.clone(), ViewState::default(), false, window, cx);
                    },
                )))
            })
    }

    /// Database objects, each enabled only where it can be created.
    fn render_schema_group(&self, cx: &mut Context<Self>) -> ToolbarGroup {
        let targets = self.schema_targets(cx);

        ToolbarGroup::new("toolbar-schema")
            .label("Database objects")
            .child(
                Button::new("new-database")
                    .ghost()
                    .icon(entry_icon(EntryKind::Database, cx))
                    .tooltip("New Database")
                    .disabled(targets.database.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.new_database(window, cx))),
            )
            .child(
                Button::new("new-table")
                    .ghost()
                    .icon(object_icon(ObjectKind::Table, cx))
                    .tooltip("New Table")
                    .disabled(targets.table.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.new_table(window, cx))),
            )
            .child(
                Button::new("new-column")
                    .ghost()
                    .icon(Icon::new(LucideIcon::Columns3).text_color(cx.theme().yellow))
                    .tooltip("New Column")
                    .disabled(targets.column.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.new_column(window, cx))),
            )
    }

    /// Workspace files and connections.
    fn render_workspace_group(&self, cx: &mut Context<Self>) -> ToolbarGroup {
        ToolbarGroup::new("toolbar-workspace")
            .label("Workspace")
            .child(
                Button::new("new-query")
                    .ghost()
                    .icon(Icon::new(LucideIcon::ScrollText).text_color(cx.theme().magenta))
                    .tooltip("New Temporary Query")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.new_temporary_query(window, cx)),
                    ),
            )
            .child(
                Button::new("new-sql-document")
                    .ghost()
                    .icon(document_icon(DocumentKind::Sql, cx))
                    .tooltip("New SQL Document")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.new_document(DocumentKind::Sql, None, window, cx)
                    })),
            )
            .child(
                Button::new("new-note")
                    .ghost()
                    .icon(document_icon(DocumentKind::Markdown, cx))
                    .tooltip("New Note")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.new_document(DocumentKind::Markdown, None, window, cx)
                    })),
            )
            .child(
                Button::new("new-folder")
                    .ghost()
                    .icon(Icon::new(LucideIcon::FolderPlus).text_color(cx.theme().yellow))
                    .tooltip("New Folder")
                    .on_click(cx.listener(|this, _, window, cx| this.new_folder(window, cx))),
            )
            .child(
                Button::new("new-connection")
                    .ghost()
                    .icon(Icon::new(LucideIcon::Plug).text_color(cx.theme().blue))
                    .tooltip("New Connection")
                    .on_click(cx.listener(|this, _, window, cx| {
                        connection_dialog::open(this.model.clone(), None, window, cx)
                    })),
            )
    }

    /// List, icons, or relationship graph; the graph only where the explorer
    /// lists tables or views.
    fn render_layout_buttons(&self, cx: &mut Context<Self>) -> Vec<Button> {
        let Some(TabContent::Explorer(explorer)) = self.focused_content() else {
            return Vec::new();
        };

        let explorer = explorer.read(cx);
        let current = explorer.effective_layout();
        let can_show_graph = explorer.can_show_graph();

        [
            (
                "layout-details",
                LucideIcon::List,
                "Show as list",
                ExplorerLayout::Details,
            ),
            (
                "layout-icons",
                LucideIcon::LayoutGrid,
                "Show as icons",
                ExplorerLayout::Icons,
            ),
            (
                "layout-graph",
                LucideIcon::Network,
                "Show relationships",
                ExplorerLayout::Graph,
            ),
        ]
        .into_iter()
        .map(|(id, icon, tooltip, layout)| {
            Button::new(id)
                .ghost()
                .icon(Icon::new(icon))
                .tooltip(tooltip)
                .toggled(current == layout)
                .disabled(layout == ExplorerLayout::Graph && !can_show_graph)
                .on_click(cx.listener(move |this, _, _, cx| this.set_layout(layout, cx)))
        })
        .collect()
    }
}

fn render_application_menu(action_focus: FocusHandle) -> impl IntoElement {
    Button::new("application-menu")
        .ghost()
        .small()
        .icon(IconName::Menu)
        .tooltip("Application menu")
        .accessibility_label("Application menu")
        .dropdown_menu(move |menu, window, cx| {
            let items = cx
                .get_menus()
                .unwrap_or_default()
                .into_iter()
                .map(OwnedMenuItem::Submenu);

            items.fold(menu.action_context(action_focus.clone()), |menu, item| {
                append_application_menu_item(menu, item, window, cx)
            })
        })
}

fn append_application_menu_item(
    menu: PopupMenu,
    item: OwnedMenuItem,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    match item {
        OwnedMenuItem::Separator => menu.separator(),
        OwnedMenuItem::Action {
            name,
            action,
            checked,
            disabled,
            ..
        } => menu.item(
            PopupMenuItem::new(name)
                .action(action)
                .checked(checked)
                .disabled(disabled),
        ),
        OwnedMenuItem::Submenu(submenu) if submenu.disabled => {
            menu.item(PopupMenuItem::new(submenu.name).disabled(true))
        }
        OwnedMenuItem::Submenu(submenu) => {
            menu.submenu(submenu.name, window, cx, move |menu, window, cx| {
                submenu.items.iter().cloned().fold(menu, |menu, item| {
                    append_application_menu_item(menu, item, window, cx)
                })
            })
        }
        // System-owned menus such as macOS Services have no in-window equivalent.
        OwnedMenuItem::SystemMenu(_) => menu,
    }
}

/// Set apart at the far end: about the app rather than the workspace.
fn render_app_group(shell_focus: FocusHandle) -> ToolbarGroup {
    ToolbarGroup::new("toolbar-app")
        .label("Application")
        .child(
            Button::new("settings")
                .ghost()
                .icon(IconName::Settings)
                .tooltip("Settings")
                // The shell's node bubbles to the workbench, which owns the
                // theme, even when nothing inside the shell has focus.
                .on_click(move |_, window, cx| {
                    shell_focus.dispatch_action(&OpenSettings, window, cx)
                }),
        )
        .child(
            Button::new("about")
                .ghost()
                .icon(IconName::Info)
                .tooltip("About Qwerio")
                .on_click(|_, window, cx| about_dialog::open(window, cx)),
        )
}

fn group_separator() -> Separator {
    Separator::vertical().h_4().mx_1()
}
