//! Settings → Appearance: the theme picker. Choosing a theme applies and
//! saves it at once.

use gpui_kit::component::searchable_list::{SearchableGroup, SearchableListItem, SearchableVec};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    TestSupportExt as _, Window, div, px,
};

use super::{setting_card, setting_row};
use crate::theme::controller::ThemeController;
use crate::theme::{DARK_THEMES, LIGHT_THEMES, ThemePreference};

const THEME_SELECT_WIDTH: f32 = 200.;
/// Tall enough to list every theme without scrolling.
const THEME_MENU_MAX_HEIGHT: f32 = 520.;
const SYSTEM_DESCRIPTION: &str = "System follows your desktop's light or dark mode. \
     On Omarchy, it matches the active Omarchy theme.";

type ThemeOptions = SearchableVec<SearchableGroup<ThemeOption>>;

#[derive(Clone)]
struct ThemeOption(ThemePreference);

impl SearchableListItem for ThemeOption {
    type Value = ThemePreference;

    fn title(&self) -> SharedString {
        self.0.label().into()
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

fn theme_options() -> ThemeOptions {
    let group = |title: &'static str, preferences: &[ThemePreference]| {
        SearchableGroup::new(title).items(preferences.iter().copied().map(ThemeOption))
    };

    SearchableVec::new(vec![
        group("Automatic", &[ThemePreference::System]),
        group("Light", &LIGHT_THEMES),
        group("Dark", &DARK_THEMES),
    ])
}

pub struct AppearanceSettings {
    theme_select: Entity<SelectState<ThemeOptions>>,
    theme: Entity<ThemeController>,
    _subscriptions: [Subscription; 2],
}

impl AppearanceSettings {
    pub fn new(
        theme: Entity<ThemeController>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let preference = theme.read(cx).preference();
        let theme_select = cx.new(|cx| {
            let mut select = SelectState::new(theme_options(), None, window, cx);
            select.set_selected_value(&preference, window, cx);
            select
        });

        let chosen_theme = theme.clone();
        let subscriptions = [
            cx.subscribe_in(
                &theme_select,
                window,
                move |_, _, event: &SelectEvent<ThemeOptions>, window, cx| {
                    let SelectEvent::Confirm(Some(preference)) = event else {
                        return;
                    };
                    let preference = *preference;

                    chosen_theme
                        .update(cx, |theme, cx| theme.set_preference(preference, window, cx));
                },
            ),
            // The saved preference may finish loading after the dialog opens.
            cx.observe_in(&theme, window, |this, theme, window, cx| {
                let preference = theme.read(cx).preference();

                this.theme_select.update(cx, |select, cx| {
                    if select.selected_value() != Some(&preference) {
                        select.set_selected_value(&preference, window, cx);
                    }
                });
                cx.notify();
            }),
        ];

        Self {
            theme_select,
            theme,
            _subscriptions: subscriptions,
        }
    }
}

impl Render for AppearanceSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(setting_card(
                [setting_row(
                    "settings-theme-row",
                    "Theme",
                    SYSTEM_DESCRIPTION,
                    Select::new(&self.theme_select)
                        .id("settings-theme")
                        .accessibility_label("Theme")
                        .w(px(THEME_SELECT_WIDTH))
                        .menu_max_h(px(THEME_MENU_MAX_HEIGHT)),
                    cx,
                )],
                cx,
            ))
            .children(self.theme.read(cx).save_error().cloned().map(|error| {
                div()
                    .id("settings-theme-error")
                    .role(Role::Status)
                    .aria_label(error.clone())
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error)
                    .test_support()
            }))
    }
}
