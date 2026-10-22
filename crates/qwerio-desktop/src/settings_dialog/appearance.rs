//! Settings → Appearance: the theme picker and the transparent (frosted
//! glass) background with its shade. Each change applies and saves at once.

use gpui_kit::component::select::{Select, SelectDelegate, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::slider::{Slider, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, IndexPath, StyledExt as _, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, TestSupportExt as _, Window, div, px, transparent_black,
};

use super::{setting_card, setting_row};
use crate::theme::controller::ThemeController;
use crate::theme::glass::{FrostedGlass, MAX_SHADE, MIN_SHADE};
use crate::theme::{DARK_THEMES, LIGHT_THEMES, ThemePreference};

const THEME_SELECT_WIDTH: f32 = 200.;
/// Tall enough to list every theme without scrolling.
const THEME_MENU_MAX_HEIGHT: f32 = 520.;
const SYSTEM_DESCRIPTION: &str = "System follows your desktop's light or dark mode. \
     On Omarchy, it matches the active Omarchy theme.";
const TRANSPARENT_BACKGROUND_DESCRIPTION: &str = "Shows the desktop, blurred, behind the \
     window, tinted with the theme's colors. On Linux, the compositor must support blur.";
const DARKNESS_DESCRIPTION: &str = "How dark the transparent background is.";
const LIGHTNESS_DESCRIPTION: &str = "How light the transparent background is.";
const SHADE_STEP: f32 = 0.05;

#[derive(Clone)]
struct ThemeOption(ThemePreference);

impl SelectItem for ThemeOption {
    type Value = ThemePreference;

    fn title(&self) -> SharedString {
        self.0.label().into()
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

struct ThemeGroup {
    title: &'static str,
    options: Vec<ThemeOption>,
}

/// The picker's options: System, then the light and dark themes, each group
/// under its own header and divider.
struct ThemeOptions {
    groups: Vec<ThemeGroup>,
}

fn theme_options() -> ThemeOptions {
    let group = |title: &'static str, preferences: &[ThemePreference]| ThemeGroup {
        title,
        options: preferences.iter().copied().map(ThemeOption).collect(),
    };

    ThemeOptions {
        groups: vec![
            group("Automatic", &[ThemePreference::System]),
            group("Light", &LIGHT_THEMES),
            group("Dark", &DARK_THEMES),
        ],
    }
}

impl SelectDelegate for ThemeOptions {
    type Item = ThemeOption;

    fn sections_count(&self, _: &App) -> usize {
        self.groups.len()
    }

    fn items_count(&self, section: usize) -> usize {
        self.groups
            .get(section)
            .map_or(0, |group| group.options.len())
    }

    fn item(&self, ix: IndexPath) -> Option<&Self::Item> {
        self.groups.get(ix.section)?.options.get(ix.row)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        Self::Item: SelectItem<Value = V>,
        V: PartialEq,
    {
        self.groups.iter().enumerate().find_map(|(section, group)| {
            let row = group
                .options
                .iter()
                .position(|option| option.value() == value)?;

            Some(IndexPath::default().section(section).row(row))
        })
    }

    fn render_section_header(
        &self,
        section: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let group = self.groups.get(section)?;

        // The list gives every header the measured height of the first, so
        // the first keeps its divider, only transparent.
        let divider_color = if section == 0 {
            transparent_black()
        } else {
            cx.theme().border
        };

        let header = v_flex().child(div().my_1().h_px().bg(divider_color)).child(
            div()
                .px_2()
                .py_0p5()
                .text_xs()
                .font_medium()
                .text_color(cx.theme().muted_foreground)
                .child(group.title.to_uppercase()),
        );

        Some(header.into_any_element())
    }
}

pub struct AppearanceSettings {
    theme_select: Entity<SelectState<ThemeOptions>>,
    shade_slider: Entity<SliderState>,
    theme: Entity<ThemeController>,
    _subscriptions: [Subscription; 3],
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

        let shade = theme.read(cx).frosted_glass().shade;
        let shade_slider = cx.new(|_| {
            SliderState::new()
                .min(MIN_SHADE)
                .max(MAX_SHADE)
                .step(SHADE_STEP)
                .default_value(shade)
        });

        let chosen_theme = theme.clone();
        let shaded_theme = theme.clone();
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
            // Observed rather than subscribed: keyboard and accessibility
            // steps change the value without an event.
            cx.observe_in(&shade_slider, window, move |_, slider, window, cx| {
                let shade = slider.read(cx).value().end();
                let frosted_glass = shaded_theme.read(cx).frosted_glass();
                if frosted_glass.shade == shade {
                    return;
                }

                shaded_theme.update(cx, |theme, cx| {
                    theme.set_frosted_glass(
                        FrostedGlass {
                            shade,
                            ..frosted_glass
                        },
                        window,
                        cx,
                    )
                });
            }),
            // The saved settings may finish loading after the dialog opens.
            cx.observe_in(&theme, window, |this, theme, window, cx| {
                let preference = theme.read(cx).preference();
                let shade = theme.read(cx).frosted_glass().shade;

                this.theme_select.update(cx, |select, cx| {
                    if select.selected_value() != Some(&preference) {
                        select.set_selected_value(&preference, window, cx);
                    }
                });
                this.shade_slider.update(cx, |slider, cx| {
                    if slider.value().end() != shade {
                        slider.set_value(shade, window, cx);
                    }
                });
                cx.notify();
            }),
        ];

        Self {
            theme_select,
            shade_slider,
            theme,
            _subscriptions: subscriptions,
        }
    }
}

impl AppearanceSettings {
    /// Darkness in dark themes, lightness in light ones: the way the shade
    /// moves the background.
    fn render_shade_row(&self, cx: &App) -> AnyElement {
        let (title, description) = if cx.theme().is_dark() {
            ("Darkness", DARKNESS_DESCRIPTION)
        } else {
            ("Lightness", LIGHTNESS_DESCRIPTION)
        };

        setting_row(
            "settings-frosted-glass-shade-row",
            title,
            description,
            div()
                .id("settings-frosted-glass-shade")
                .w(px(THEME_SELECT_WIDTH))
                .child(Slider::new(&self.shade_slider).accessibility_label(title))
                .test_support(),
            cx,
        )
    }
}

impl Render for AppearanceSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let frosted_glass = self.theme.read(cx).frosted_glass();
        let transparent_background = Switch::new("settings-frosted-glass")
            .accessibility_label("Transparent background")
            .checked(frosted_glass.is_on)
            .on_click(move |is_on: &bool, window, cx| {
                let frosted_glass = FrostedGlass {
                    is_on: *is_on,
                    ..frosted_glass
                };
                theme.update(cx, |theme, cx| {
                    theme.set_frosted_glass(frosted_glass, window, cx)
                });
            });

        let mut rows = vec![
            setting_row(
                "settings-theme-row",
                "Theme",
                SYSTEM_DESCRIPTION,
                Select::new(&self.theme_select)
                    .id("settings-theme")
                    .accessibility_label("Theme")
                    .w(px(THEME_SELECT_WIDTH))
                    .menu_max_h(px(THEME_MENU_MAX_HEIGHT)),
                cx,
            ),
            setting_row(
                "settings-frosted-glass-row",
                "Transparent background",
                TRANSPARENT_BACKGROUND_DESCRIPTION,
                transparent_background,
                cx,
            ),
        ];
        if frosted_glass.is_on {
            rows.push(self.render_shade_row(cx));
        }

        v_flex().gap_3().child(setting_card(rows, cx)).children(
            self.theme.read(cx).save_error().cloned().map(|error| {
                div()
                    .id("settings-theme-error")
                    .role(Role::Status)
                    .aria_label(error.clone())
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error)
                    .test_support()
            }),
        )
    }
}
