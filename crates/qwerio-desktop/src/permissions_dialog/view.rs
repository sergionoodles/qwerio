//! The dialog's layout: the list of users and roles, the chosen pane, and
//! the bar that reviews and applies staged changes.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
    px,
};

use super::*;

const BODY_HEIGHT: f32 = 520.;
const SQL_LIST_HEIGHT: f32 = 132.;
const AVATAR_SIZE: f32 = 40.;

/// A titled group of content with a one-line explanation.
pub(super) fn section(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    cx: &App,
) -> gpui_kit::Div {
    v_flex().gap_3().child(
        v_flex()
            .gap_0p5()
            .child(div().text_sm().font_semibold().child(title.into()))
            .child(muted(description, cx)),
    )
}

pub(super) fn muted(text: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

pub(super) fn card(cx: &App) -> gpui_kit::Div {
    v_flex()
        .w_full()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(cx.theme().border)
}

/// A large icon in a circle, beside a title and its details.
pub(super) fn heading(
    icon: LucideIcon,
    title: impl Into<SharedString>,
    tag: Option<&'static str>,
    detail: impl Into<SharedString>,
    cx: &App,
) -> impl IntoElement {
    let title: SharedString = title.into();
    h_flex()
        .gap_3()
        .items_center()
        .child(
            div()
                .size(px(AVATAR_SIZE))
                .flex_none()
                .rounded_full()
                .bg(cx.theme().muted)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).text_color(cx.theme().muted_foreground)),
        )
        .child(
            v_flex()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .id("permission-heading")
                                .aria_label(title.clone())
                                .text_lg()
                                .font_semibold()
                                .truncate()
                                .child(title)
                                .test_support(),
                        )
                        .children(tag.map(|tag| Tag::secondary().small().child(tag))),
                )
                .child(muted(detail, cx)),
        )
}

pub(super) fn sql_block(
    id: &'static str,
    sql: SharedString,
    is_valid: bool,
    cx: &App,
) -> AnyElement {
    let color = if is_valid {
        cx.theme().foreground
    } else {
        cx.theme().danger
    };
    div()
        .id(id)
        .w_full()
        .px_3()
        .py_2()
        .rounded(cx.theme().radius)
        .bg(cx.theme().muted)
        .font_family(cx.theme().mono_font_family.clone())
        .text_xs()
        .text_color(color)
        .aria_label(sql.clone())
        .child(sql)
        .test_support()
        .into_any_element()
}

impl PermissionsForm {
    fn render_messages(&self) -> impl IntoElement {
        let warnings = self
            .snapshot
            .iter()
            .flat_map(|snapshot| snapshot.warnings.iter())
            .enumerate()
            .map(|(index, warning)| {
                Alert::warning(("permission-warning", index), warning.clone()).small()
            });

        v_flex()
            .gap_2()
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .id("permission-error")
                        .aria_label(error.clone())
                        .child(Alert::error("permission-error-alert", error).small())
                        .test_support(),
                )
            })
            .when_some(self.notice.clone(), |view, notice| {
                view.child(
                    div()
                        .id("permission-notice")
                        .aria_label(notice.clone())
                        .child(Alert::success("permission-notice-alert", notice).small())
                        .test_support(),
                )
            })
            .children(warnings)
    }

    fn render_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.pane.clone() {
            Pane::Overview => self.render_overview(cx),
            Pane::Identity(principal) => self.render_identity(&principal, cx),
            Pane::NewUser | Pane::NewRole => self.render_create(cx),
        }
    }

    fn render_pending_sql(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let statements: Vec<_> = self
            .pending
            .iter()
            .map(|change| self.preview(&change.to_change()))
            .collect();
        let label: SharedString = statements
            .iter()
            .map(|statement| statement.clone().unwrap_or_else(|error| error))
            .collect::<Vec<_>>()
            .join("\n")
            .into();
        let rows =
            self.pending
                .iter()
                .zip(statements)
                .enumerate()
                .map(|(index, (change, statement))| {
                    let change = change.clone();
                    let is_valid = statement.is_ok();
                    let text = statement.unwrap_or_else(|error| error);
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_xs()
                                .when(!is_valid, |text| text.text_color(cx.theme().danger))
                                .child(text),
                        )
                        .child(
                            Button::new(("permission-unstage", index))
                                .ghost()
                                .xsmall()
                                .icon(LucideIcon::X)
                                .accessibility_label("Remove this change")
                                .tooltip("Remove this change")
                                .disabled(self.is_busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.stage(change.clone(), cx)
                                })),
                        )
                });

        div()
            .id("permission-pending-sql")
            .aria_label(label)
            .rounded(cx.theme().radius)
            .bg(cx.theme().muted)
            .child(
                v_flex()
                    .id("permission-pending-sql-list")
                    .max_h(px(SQL_LIST_HEIGHT))
                    .gap_1()
                    .px_3()
                    .py_2()
                    .children(rows)
                    .overflow_y_scrollbar(),
            )
            .test_support()
    }

    fn render_pending_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.pending.len();
        let has_pending = count > 0;
        let has_invalid = self
            .pending
            .iter()
            .any(|change| self.preview(&change.to_change()).is_err());
        let summary: SharedString = match count {
            0 => "No pending changes. Changes you make are listed here before they run.".into(),
            1 => "1 change ready to apply".into(),
            count => format!("{count} changes ready to apply").into(),
        };

        v_flex()
            .id("permission-pending")
            .mt_4()
            .pt_3()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .when(has_pending && self.shows_sql, |bar| {
                bar.child(self.render_pending_sql(cx))
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .id("permission-pending-summary")
                            .flex_1()
                            .text_sm()
                            .when(!has_pending, |text| {
                                text.text_color(cx.theme().muted_foreground)
                            })
                            .aria_label(summary.clone())
                            .child(summary)
                            .test_support(),
                    )
                    .when(has_pending, |bar| {
                        bar.child(
                            Button::new("permission-toggle-sql")
                                .ghost()
                                .small()
                                .icon(LucideIcon::Code)
                                .label(if self.shows_sql {
                                    "Hide SQL"
                                } else {
                                    "Review SQL"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.shows_sql = !this.shows_sql;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("permission-discard")
                                .ghost()
                                .small()
                                .label("Discard")
                                .disabled(self.is_busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.pending.clear();
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Button::new("permission-apply")
                            .primary()
                            .small()
                            .label("Apply changes")
                            .loading(self.is_busy && has_pending)
                            .disabled(self.is_busy || !has_pending || has_invalid)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.apply_pending(window, cx)),
                            ),
                    ),
            )
    }
}

impl Render for PermissionsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("permissions-manager")
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .h(px(BODY_HEIGHT))
                    .items_start()
                    .child(self.render_sidebar(cx))
                    .child(
                        v_flex()
                            .id("permission-main")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .pl_5()
                            .pr_1()
                            .gap_4()
                            .child(self.render_messages())
                            .child(self.render_pane(cx))
                            .overflow_y_scrollbar(),
                    ),
            )
            .child(self.render_pending_bar(cx))
    }
}
