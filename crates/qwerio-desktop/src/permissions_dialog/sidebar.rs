//! The searchable list of the server's users and roles.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::Input;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Div, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, div,
    px,
};
use qwerio_core::permissions::PrincipalInfo;

use super::directory::{self, Group};
use super::view::muted;
use super::*;

const SIDEBAR_WIDTH: f32 = 248.;
const PENDING_DOT_SIZE: f32 = 6.;

pub(super) fn principal_icon(info: &PrincipalInfo) -> LucideIcon {
    if directory::is_role(info) {
        LucideIcon::ShieldUser
    } else {
        LucideIcon::User
    }
}

/// A left-aligned, clickable row. Buttons center their content, which suits
/// actions but not a list.
fn nav_row(id: impl Into<ElementId>, is_selected: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let (hover, active) = (theme.list_hover, theme.list_active);
    h_flex()
        .id(id)
        .w_full()
        .gap_2()
        .px_2()
        .py_1()
        .rounded(theme.radius)
        .text_sm()
        .cursor_pointer()
        .when(is_selected, |row| row.bg(active))
        .when(!is_selected, |row| row.hover(|style| style.bg(hover)))
}

impl PermissionsForm {
    fn has_pending_for(&self, principal: &Principal) -> bool {
        self.pending.iter().any(|change| match change {
            PendingChange::Privilege {
                principal: target, ..
            } => target == principal,
            PendingChange::Membership { role, member, .. }
            | PendingChange::DefaultRole { role, member } => {
                role == principal || member == principal
            }
        })
    }

    fn render_principal(&self, info: &PrincipalInfo, cx: &mut Context<Self>) -> AnyElement {
        let principal = info.principal.clone();
        let is_selected = self.pane == Pane::Identity(principal.clone());
        let id = SharedString::from(format!("permission-principal-{}", principal.label()));
        let host = principal.host.clone().filter(|host| !host.is_empty());

        nav_row(id, is_selected, cx)
            .aria_label(principal.label())
            .child(
                Icon::new(principal_icon(info))
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(div().min_w_0().truncate().child(principal.name.clone()))
            .children(host.map(|host| {
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("@{host}"))
            }))
            .child(div().flex_1())
            .when(self.has_pending_for(&principal), |row| {
                row.child(
                    div()
                        .size(px(PENDING_DOT_SIZE))
                        .flex_none()
                        .rounded_full()
                        .bg(cx.theme().primary),
                )
            })
            .on_click(
                cx.listener(move |this, _, _, cx| this.show(Pane::Identity(principal.clone()), cx)),
            )
            .into_any_element()
    }

    fn render_group(&self, group: Group, query: &str, cx: &mut Context<Self>) -> AnyElement {
        let principals = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.principals.as_slice())
            .unwrap_or_default();
        let members = directory::members(self.engine, principals, group, query);
        let is_collapsible = group == Group::System && query.trim().is_empty();
        let is_expanded = !is_collapsible || self.shows_built_in;

        if members.is_empty() && group == Group::System {
            return div().into_any_element();
        }

        let title = format!("{} · {}", group.label(), members.len());
        let header = if is_collapsible {
            let chevron = if is_expanded {
                LucideIcon::ChevronDown
            } else {
                LucideIcon::ChevronRight
            };
            h_flex()
                .id("permission-toggle-built-in")
                .gap_1()
                .px_2()
                .text_xs()
                .font_medium()
                .text_color(cx.theme().muted_foreground)
                .cursor_pointer()
                .hover(|style| style.text_color(cx.theme().foreground))
                .child(Icon::new(chevron).xsmall())
                .child(title)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.shows_built_in = !this.shows_built_in;
                    cx.notify();
                }))
                .into_any_element()
        } else {
            div()
                .px_2()
                .text_xs()
                .font_medium()
                .text_color(cx.theme().muted_foreground)
                .child(title)
                .into_any_element()
        };
        let items: Vec<_> = members
            .into_iter()
            .map(|info| self.render_principal(info, cx))
            .collect();

        v_flex()
            .gap_0p5()
            .child(header)
            .when(is_expanded, |group| group.children(items))
            .into_any_element()
    }

    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = Self::text(&self.search, cx);
        let is_overview = self.pane == Pane::Overview;
        let groups: Vec<_> = Group::ALL
            .into_iter()
            .map(|group| self.render_group(group, &query, cx))
            .collect();
        let is_empty = self.snapshot.is_none() && !self.is_busy;

        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .gap_3()
            .pr_4()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                Input::new(&self.search)
                    .id("permission-search")
                    .small()
                    .cleanable(true)
                    .prefix(
                        Icon::new(LucideIcon::Search)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .child(
                v_flex()
                    .id("permission-principals")
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .child(
                        nav_row("permission-overview", is_overview, cx)
                            .child(
                                Icon::new(LucideIcon::ShieldCheck)
                                    .small()
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child("Who has access")
                            .on_click(cx.listener(|this, _, _, cx| this.show(Pane::Overview, cx))),
                    )
                    .when(is_empty, |list| {
                        list.child(
                            muted("Users and roles appear here once the server responds.", cx)
                                .px_2(),
                        )
                    })
                    .children(groups)
                    .overflow_y_scrollbar(),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("permission-new-user")
                            .outline()
                            .small()
                            .flex_1()
                            .icon(LucideIcon::UserPlus)
                            .label("New user")
                            .disabled(self.is_busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_create(false, window, cx)
                            })),
                    )
                    .when(self.supports_roles(), |row| {
                        row.child(
                            Button::new("permission-new-role")
                                .outline()
                                .small()
                                .flex_1()
                                .icon(LucideIcon::ShieldUser)
                                .label("New role")
                                .disabled(self.is_busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_create(true, window, cx)
                                })),
                        )
                    }),
            )
    }
}
