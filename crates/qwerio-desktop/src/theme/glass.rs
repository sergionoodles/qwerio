//! Frosted glass: an optional see-through window. The desktop shows through,
//! blurred, behind a translucent window background in the theme's own color,
//! so every theme keeps its palette. Text, icons, accents, borders, popovers,
//! and dialogs stay opaque.
//!
//! The window background is painted once, under everything. Surfaces that
//! repeat it, such as lists, tables, and editors, turn clear so the glass is
//! not layered twice; surfaces with a tone of their own, such as the sidebar
//! and headers, keep a light tint of it.
//!
//! The blur itself comes from the platform: macOS and Windows always, Linux
//! when the compositor blurs translucent windows. Without blur the window is
//! still translucent.

use std::sync::Arc;

use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::{ActiveTheme as _, Theme, WindowExt as _};
use gpui_kit::{App, Global, Hsla, Styled as _, Window, transparent_black};

/// Settings key for whether frosted glass is on.
pub const FROSTED_GLASS_SETTING: &str = "frosted_glass";

/// Settings key for the frosted glass [`FrostedGlass::shade`].
pub const FROSTED_GLASS_SHADE_SETTING: &str = "frosted_glass_shade";

/// The shade range: from the theme's own background to the most shaded.
pub const MIN_SHADE: f32 = 0.;
pub const MAX_SHADE: f32 = 1.;

/// Midway: the glass lifts dark backgrounds, so they start a touch darker.
const DEFAULT_SHADE: f32 = 0.5;

/// How much of the theme's background covers the desktop, from no shade to
/// full shade. Clear enough to see the desktop, never so clear that text
/// over a busy wallpaper becomes hard to read.
const MIN_BACKGROUND_OPACITY: f32 = 0.7;
const MAX_BACKGROUND_OPACITY: f32 = 0.88;
/// How much of its own tone a surface lays over the glass.
const TINT_OPACITY: f32 = 0.5;
/// At full shade, dark backgrounds lose this share of their lightness, and
/// light backgrounds move this share of the way to white. Enough to tune
/// the main view without losing the theme's color.
const MAX_SHADE_SHIFT: f32 = 0.4;

/// What Settings → Appearance sets for frosted glass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrostedGlass {
    pub is_on: bool,
    /// How far the window background moves from the theme's: darker in dark
    /// themes, lighter in light ones. From [`MIN_SHADE`] to [`MAX_SHADE`].
    pub shade: f32,
}

impl Default for FrostedGlass {
    fn default() -> Self {
        Self {
            is_on: false,
            shade: DEFAULT_SHADE,
        }
    }
}

/// The theme's opaque window background, kept for dialogs while frosted
/// glass shows a translucent one.
pub(super) struct OpaqueBackground(pub(super) Hsla);

impl Global for OpaqueBackground {}

/// Turns `theme`'s surfaces translucent, its window background shaded by
/// `shade`. Apply it to a freshly installed theme, never twice.
pub fn apply_frosted_glass(theme: &mut Theme, shade: f32) {
    // Editors without their own color follow the input background, which in
    // light themes is the window background.
    let editor = theme
        .highlight_theme
        .style
        .editor_background
        .unwrap_or_else(|| theme.input_background());
    let background = theme.background;
    let is_dark = theme.mode.is_dark();
    let colors = &mut theme.colors;
    let tokens = &mut theme.tokens;

    // Each token is set with its color: re-applying frosted glass leaves a
    // color as it was, and the theme then takes the reinstalled opaque token.
    for (color, token) in [
        (&mut colors.sidebar, &mut tokens.sidebar),
        (&mut colors.title_bar, &mut tokens.title_bar),
        (&mut colors.status_bar, &mut tokens.status_bar),
        (&mut colors.tab_bar, &mut tokens.tab_bar),
        (&mut colors.list, &mut tokens.list),
        (&mut colors.list_even, &mut tokens.list_even),
        (&mut colors.list_head, &mut tokens.list_head),
        (&mut colors.table, &mut tokens.table),
        (&mut colors.table_even, &mut tokens.table_even),
        (&mut colors.table_head, &mut tokens.table_head),
        (&mut colors.table_foot, &mut tokens.table_foot),
    ] {
        *color = frost_surface(*color, background);
        *token = (*color).into();
    }

    // The active tab sits on the tab bar, not on the window background, so it
    // keeps its tint even when it matches the background.
    colors.tab_active = tint(colors.tab_active);
    tokens.tab_active = colors.tab_active.into();
    colors.background = shade_background(background, is_dark, shade);
    tokens.background = colors.background.into();

    let mut highlight = (*theme.highlight_theme).clone();
    highlight.style.editor_background = Some(frost_surface(editor, background));
    theme.highlight_theme = Arc::new(highlight);
}

/// Opens a dialog. Dialogs float over the window's content, which would show
/// through them, so they keep the theme's opaque background under frosted
/// glass. Open every dialog through here.
pub fn open_dialog(
    window: &mut Window,
    cx: &mut App,
    build: impl Fn(Dialog, &mut Window, &mut App) -> Dialog + 'static,
) {
    window.open_dialog(cx, move |dialog, window, cx| {
        let background = cx
            .try_global::<OpaqueBackground>()
            .map_or(cx.theme().background, |opaque| opaque.0);
        build(dialog.bg(background), window, cx)
    });
}

/// Clear where the window background already shows through, a tint otherwise.
fn frost_surface(color: Hsla, background: Hsla) -> Hsla {
    if color == background {
        return transparent_black();
    }

    tint(color)
}

/// `background` darker in dark themes, lighter in light ones, and less
/// see-through the more it is shaded. The hue and saturation stay, so it is
/// still the theme's color.
fn shade_background(background: Hsla, is_dark: bool, shade: f32) -> Hsla {
    let shade = shade.clamp(MIN_SHADE, MAX_SHADE);
    let shift = shade * MAX_SHADE_SHIFT;
    let lightness = if is_dark {
        background.l * (1. - shift)
    } else {
        background.l + (1. - background.l) * shift
    };

    let opacity = MIN_BACKGROUND_OPACITY
        + (MAX_BACKGROUND_OPACITY - MIN_BACKGROUND_OPACITY) * shade / MAX_SHADE;

    Hsla {
        l: lightness,
        a: opacity,
        ..background
    }
}

/// A color that is already see-through is a tint by design and stays as is.
fn tint(color: Hsla) -> Hsla {
    if color.a < 1. {
        return color;
    }

    color.opacity(TINT_OPACITY)
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use gpui_kit::component::ThemeSet;
    use gpui_kit::hsla;

    use super::*;

    fn bundled_theme(name: &str) -> Theme {
        let config = [super::super::QWERIO_THEMES, super::super::NAMED_THEMES]
            .into_iter()
            .flat_map(|text| {
                serde_json::from_str::<ThemeSet>(text)
                    .expect("bundled theme set")
                    .themes
            })
            .find(|theme| theme.name.as_ref() == name)
            .expect("a bundled theme");

        let mut theme = Theme::default();
        theme.apply_config(&Rc::new(config));
        theme
    }

    fn same_color(left: Hsla, right: Hsla) -> bool {
        (left.h, left.s, left.l) == (right.h, right.s, right.l)
    }

    #[test]
    fn the_background_keeps_the_theme_color_but_lets_the_desktop_through() {
        let opaque = bundled_theme("Tokyo Night");
        let mut frosted = opaque.clone();

        apply_frosted_glass(&mut frosted, MIN_SHADE);

        assert!(same_color(frosted.background, opaque.background));
        assert!(frosted.background.a > 0. && frosted.background.a < 1.);
        assert_eq!(frosted.background.alpha(1.), opaque.background);
    }

    #[test]
    fn surfaces_matching_the_background_clear_and_others_keep_a_tint() {
        let opaque = bundled_theme("Tokyo Night");
        let mut frosted = opaque.clone();

        apply_frosted_glass(&mut frosted, DEFAULT_SHADE);

        assert_eq!(opaque.table, opaque.background);
        assert_eq!(frosted.table.a, 0.);
        assert_ne!(opaque.sidebar, opaque.background);
        assert!(same_color(frosted.sidebar, opaque.sidebar));
        assert!(frosted.sidebar.a > 0. && frosted.sidebar.a < 1.);
        assert!(frosted.tab_active.a > 0.);
    }

    #[test]
    fn dark_themes_get_a_slightly_darker_main_view() {
        let opaque = bundled_theme("Tokyo Night");
        let mut frosted = opaque.clone();

        apply_frosted_glass(&mut frosted, FrostedGlass::default().shade);

        assert!(frosted.background.l < opaque.background.l);
        assert!(frosted.background.l > opaque.background.l * 0.5);
        assert_eq!(
            (frosted.background.h, frosted.background.s),
            (opaque.background.h, opaque.background.s)
        );
    }

    #[test]
    fn light_themes_lighten_with_the_shade() {
        let opaque = bundled_theme("Catppuccin Latte");
        let mut unshaded = opaque.clone();
        let mut shaded = opaque.clone();

        apply_frosted_glass(&mut unshaded, MIN_SHADE);
        apply_frosted_glass(&mut shaded, MAX_SHADE);

        assert!(same_color(unshaded.background, opaque.background));
        assert!(shaded.background.l > opaque.background.l);
        assert!(shaded.background.l < 1.);
    }

    #[test]
    fn the_shade_stays_within_its_range() {
        let opaque = bundled_theme("Tokyo Night");
        let mut darkest = opaque.clone();
        let mut beyond = opaque.clone();

        apply_frosted_glass(&mut darkest, MAX_SHADE);
        apply_frosted_glass(&mut beyond, MAX_SHADE * 10.);

        assert_eq!(beyond.background, darkest.background);
        assert!(
            darkest.background.l >= opaque.background.l * (1. - MAX_SHADE_SHIFT) - f32::EPSILON
        );
        assert!(darkest.background.a <= MAX_BACKGROUND_OPACITY);
    }

    #[test]
    fn more_shade_lets_less_of_the_desktop_through() {
        let opaque = bundled_theme("Catppuccin Latte");
        let mut unshaded = opaque.clone();
        let mut shaded = opaque.clone();

        apply_frosted_glass(&mut unshaded, MIN_SHADE);
        apply_frosted_glass(&mut shaded, MAX_SHADE);

        assert!(unshaded.background.a >= MIN_BACKGROUND_OPACITY);
        assert!(shaded.background.a > unshaded.background.a);
        assert!(shaded.background.a < 1.);
    }

    #[test]
    fn text_accents_and_popovers_stay_opaque() {
        let opaque = bundled_theme("Catppuccin Latte");
        let mut frosted = opaque.clone();

        apply_frosted_glass(&mut frosted, DEFAULT_SHADE);

        assert_eq!(frosted.foreground, opaque.foreground);
        assert_eq!(frosted.primary, opaque.primary);
        assert_eq!(frosted.border, opaque.border);
        assert_eq!(frosted.popover, opaque.popover);
    }

    #[test]
    fn light_editors_clear_instead_of_doubling_the_glass() {
        let mut frosted = bundled_theme("GitHub Light");

        apply_frosted_glass(&mut frosted, DEFAULT_SHADE);

        let editor = frosted.highlight_theme.style.editor_background;
        assert_eq!(editor.map(|color| color.a), Some(0.));
    }

    #[test]
    fn already_translucent_surfaces_are_left_alone() {
        let see_through = hsla(0.6, 0.5, 0.5, 0.3);

        assert_eq!(tint(see_through), see_through);
    }
}
