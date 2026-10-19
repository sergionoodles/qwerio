//! The workspace toolbar: navigation and breadcrumbs, then commands grouped
//! the way file managers group them. Creating things lives in one "New"
//! menu whose items follow the focused location.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::breadcrumb::{Breadcrumb, BreadcrumbItem};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu};
use gpui_kit::component::toolbar::{Toolbar, ToolbarGroup};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FocusHandle, IntoElement, ParentElement as _, Styled as _, Window, div,
};
use qwerio_app::navigation::ViewState;
use qwerio_app::schema::SchemaTargets;
use qwerio_workspace::document::DocumentKind;
use qwerio_workspace::explorer::breadcrumbs;

use super::{TabContent, WorkspaceShell};
use crate::actions::*;
use crate::content::document_icon;
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

        let commands = ToolbarGroup::new("toolbar-commands")
            .label("Commands")
            .content(self.render_new_menu(cx))
            .child(
                Button::new("refresh")
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .tooltip("Refresh")
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
            .when_some(self.render_layout_toggle(cx), |group, toggle| {
                group.child(toggle)
            })
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
            .child(commands)
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

    /// Database objects first (each enabled only where it can be created),
    /// then workspace files, then connections.
    fn render_new_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let targets = self.schema_targets(cx);
        let focus = self.focus_handle.clone();

        Button::new("new-menu")
            .ghost()
            .small()
            .icon(IconName::Plus)
            .label("New")
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| new_menu(menu, &targets, focus.clone()))
            .into_any_element()
    }

    fn render_layout_toggle(&self, cx: &mut Context<Self>) -> Option<Button> {
        if !matches!(self.focused_content(), Some(TabContent::Explorer(_))) {
            return None;
        }

        let is_icons = self.explorer_layout == ExplorerLayout::Icons;
        let (icon, tooltip, next) = if is_icons {
            (LucideIcon::List, "Show as list", ExplorerLayout::Details)
        } else {
            (
                LucideIcon::LayoutGrid,
                "Show as icons",
                ExplorerLayout::Icons,
            )
        };

        Some(
            Button::new("toggle-layout")
                .ghost()
                .icon(Icon::new(icon))
                .tooltip(tooltip)
                .on_click(cx.listener(move |this, _, _, cx| this.set_layout(next, cx))),
        )
    }
}

fn new_menu(menu: PopupMenu, targets: &SchemaTargets, focus: FocusHandle) -> PopupMenu {
    menu.action_context(focus)
        .menu_with_icon_and_disabled(
            "Database…",
            Icon::new(LucideIcon::Database),
            Box::new(NewDatabase),
            targets.database.is_none(),
        )
        .menu_with_icon_and_disabled(
            "Table…",
            Icon::new(LucideIcon::Table),
            Box::new(NewTable),
            targets.table.is_none(),
        )
        .menu_with_icon_and_disabled(
            "Column…",
            Icon::new(LucideIcon::Columns3),
            Box::new(NewColumn),
            targets.column.is_none(),
        )
        .separator()
        .menu_with_icon(
            "Temporary Query",
            Icon::new(LucideIcon::ScrollText),
            Box::new(NewTemporaryQuery),
        )
        .menu_with_icon(
            "SQL Document",
            document_icon(DocumentKind::Sql),
            Box::new(NewSqlDocument),
        )
        .menu_with_icon(
            "Note",
            document_icon(DocumentKind::Markdown),
            Box::new(NewNote),
        )
        .menu_with_icon(
            "Folder",
            Icon::new(LucideIcon::FolderPlus),
            Box::new(NewFolder),
        )
        .separator()
        .menu_with_icon(
            "Connection…",
            Icon::new(LucideIcon::Plug),
            Box::new(NewConnection),
        )
}
