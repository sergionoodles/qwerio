//! The Settings dialog: a list of sections beside the chosen section's
//! settings. A new section adds a [`SettingsSection`] variant and its view.
//!
//! The body keeps one size for every section so switching sections never
//! resizes the dialog; a section taller than the body scrolls.

mod appearance;
pub(crate) mod labels;

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogClose;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px,
};

use crate::theme::controller::ThemeController;
use appearance::AppearanceSettings;

const DIALOG_WIDTH: f32 = 720.;
const NAVIGATION_WIDTH: f32 = 176.;
const BODY_HEIGHT: f32 = 380.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsSection {
    Appearance,
    Labels,
}

impl SettingsSection {
    const ALL: [Self; 2] = [Self::Appearance, Self::Labels];

    fn id(self) -> &'static str {
        match self {
            Self::Appearance => "settings-section-appearance",
            Self::Labels => "settings-section-labels",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Labels => "Labels",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Appearance => "How Qwerio looks. Changes apply right away.",
            Self::Labels => {
                "Labels organize connections and database objects. Assign them from \
                 an item's context menu."
            }
        }
    }

    fn icon(self) -> LucideIcon {
        match self {
            Self::Appearance => LucideIcon::Palette,
            Self::Labels => LucideIcon::Tag,
        }
    }
}

/// Opens the dialog; Escape, the close button, or Done dismiss it.
pub fn open(
    theme: Entity<ThemeController>,
    model: Option<Entity<crate::workspace_model::WorkspaceModel>>,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| SettingsView::new(theme, model, window, cx));

    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title("Settings")
            .w(px(DIALOG_WIDTH))
            .child(view.clone())
            .footer(
                // DialogClose fills its parent, so the wrapper keeps the
                // button at its own width.
                h_flex()
                    .w_full()
                    .justify_end()
                    .child(
                        div().flex_none().child(
                            DialogClose::new()
                                .child(Button::new("settings-close").primary().label("Done")),
                        ),
                    ),
            )
    });
}

/// A bordered group of [`setting_row`]s divided by rules.
fn setting_card(rows: impl IntoIterator<Item = AnyElement>, cx: &App) -> impl IntoElement {
    let border = cx.theme().border;
    let rows = rows.into_iter().enumerate().map(|(index, row)| {
        div()
            .w_full()
            .when(index > 0, |this| this.border_t_1().border_color(border))
            .child(row)
    });

    v_flex()
        .w_full()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(border)
        .children(rows)
}

/// One setting: its name and an explanation beside the control that
/// changes it.
fn setting_row(
    id: &'static str,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> AnyElement {
    h_flex()
        .id(id)
        .w_full()
        .gap_6()
        .px_4()
        .py_3()
        .items_center()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(div().text_sm().font_medium().child(title.into()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description.into()),
                ),
        )
        // Controls such as Select grow inside a flex row and would squeeze
        // the description to one character per line.
        .child(div().flex_none().child(control))
        .test_support()
        .into_any_element()
}

struct SettingsView {
    section: SettingsSection,
    appearance: Entity<AppearanceSettings>,
    labels: Entity<labels::LabelSettings>,
}

impl SettingsView {
    fn new(
        theme: Entity<ThemeController>,
        model: Option<Entity<crate::workspace_model::WorkspaceModel>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            section: SettingsSection::Appearance,
            appearance: cx.new(|cx| AppearanceSettings::new(theme, window, cx)),
            labels: cx.new(|cx| labels::LabelSettings::new(model, window, cx)),
        }
    }

    fn render_navigation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items = SettingsSection::ALL.into_iter().map(|section| {
            Button::new(section.id())
                .ghost()
                .w_full()
                .selected(section == self.section)
                .accessibility_label(section.label())
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            Icon::new(section.icon())
                                .small()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(section.label()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.section = section;
                    cx.notify();
                }))
        });

        v_flex()
            .w(px(NAVIGATION_WIDTH))
            .h_full()
            .flex_none()
            .gap_0p5()
            .pr_3()
            .border_r_1()
            .border_color(cx.theme().border)
            .children(items)
    }

    fn render_header(&self, cx: &App) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(
                div()
                    .id("settings-section-title")
                    .role(Role::Heading)
                    .aria_label(self.section.label())
                    .text_lg()
                    .font_semibold()
                    .child(self.section.label())
                    .test_support(),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.section.description()),
            )
    }

    fn render_section(&self) -> AnyElement {
        match self.section {
            SettingsSection::Appearance => self.appearance.clone().into_any_element(),
            SettingsSection::Labels => self.labels.clone().into_any_element(),
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .h(px(BODY_HEIGHT))
            .items_start()
            .gap_5()
            .child(self.render_navigation(cx))
            .child(
                v_flex()
                    .id("settings-section")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .gap_4()
                    .pr_1()
                    .child(self.render_header(cx))
                    .child(self.render_section())
                    .overflow_y_scrollbar(),
            )
    }
}
