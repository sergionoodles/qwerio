//! Theme preferences: follow the system, or pick one of the bundled named
//! themes. Each resolves to a GPUI Kit theme configuration; components only
//! read semantic tokens from it.
//!
//! "System" follows the window's light or dark appearance (on Linux, the XDG
//! desktop portal's color scheme) with the Qwerio themes. Where Omarchy is
//! installed it uses the active Omarchy palette instead, so the app matches
//! the rest of the desktop.

pub mod controller;
pub mod glass;
pub mod omarchy;

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use std::io::ErrorKind;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry, ThemeSet};
use gpui_kit::{App, WindowAppearance};
use serde::{Deserialize, Serialize};

use glass::{FrostedGlass, OpaqueBackground};

const QWERIO_THEMES: &str = include_str!("../../assets/themes/qwerio.json");
const NAMED_THEMES: &str = include_str!("../../assets/themes/named.json");
const SYSTEM_LIGHT_THEME: &str = "Qwerio Light";
const SYSTEM_DARK_THEME: &str = "Qwerio Dark";

/// Settings key for the chosen [`ThemePreference`].
pub const THEME_SETTING: &str = "theme";

/// Settings key for a custom Omarchy theme directory.
pub const OMARCHY_DIRECTORY_SETTING: &str = "omarchy_theme_directory";

/// What the user picked in Settings → Appearance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemePreference {
    #[default]
    System,
    GitHubLight,
    SolarizedLight,
    CatppuccinLatte,
    AyuLight,
    TokyoNight,
    CatppuccinMocha,
    GruvboxDark,
    Nord,
    Dracula,
    AyuMirage,
}

/// Light themes in the order the picker lists them: by name.
pub const LIGHT_THEMES: [ThemePreference; 4] = [
    ThemePreference::AyuLight,
    ThemePreference::CatppuccinLatte,
    ThemePreference::GitHubLight,
    ThemePreference::SolarizedLight,
];

/// Dark themes in the order the picker lists them: by name.
pub const DARK_THEMES: [ThemePreference; 6] = [
    ThemePreference::AyuMirage,
    ThemePreference::CatppuccinMocha,
    ThemePreference::Dracula,
    ThemePreference::GruvboxDark,
    ThemePreference::Nord,
    ThemePreference::TokyoNight,
];

impl ThemePreference {
    /// The name shown to people; for named themes, also the bundled theme's name.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::GitHubLight => "GitHub Light",
            Self::SolarizedLight => "Solarized Light",
            Self::CatppuccinLatte => "Catppuccin Latte",
            Self::AyuLight => "Ayu Light",
            Self::TokyoNight => "Tokyo Night",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::GruvboxDark => "Gruvbox Dark",
            Self::Nord => "Nord",
            Self::Dracula => "Dracula",
            Self::AyuMirage => "Ayu Mirage",
        }
    }

    /// The bundled theme a named preference always shows; `None` for System.
    pub fn bundled_theme(self) -> Option<&'static str> {
        match self {
            Self::System => None,
            named => Some(named.label()),
        }
    }
}

/// The theme a preference resolves to.
#[derive(Clone, Debug)]
pub enum ThemeTarget {
    /// A theme bundled with Qwerio, by name.
    Bundled(&'static str),
    /// The active Omarchy palette.
    Omarchy(Rc<ThemeConfig>),
}

/// The Omarchy palette text: `Ok(None)` when Omarchy is not installed, `Err`
/// when it is installed but the palette cannot be read.
pub type OmarchyPalette = Result<Option<String>, String>;

/// What System shows, plus a warning when an installed Omarchy palette could
/// not be used.
pub struct SystemResolution {
    pub target: ThemeTarget,
    pub warning: Option<String>,
}

/// Resolves System: the Omarchy palette when installed and valid, otherwise
/// the Qwerio theme matching the system's light or dark appearance.
pub fn resolve_system(mode: ThemeMode, omarchy_palette: OmarchyPalette) -> SystemResolution {
    let fallback = ThemeTarget::Bundled(if mode.is_dark() {
        SYSTEM_DARK_THEME
    } else {
        SYSTEM_LIGHT_THEME
    });

    let palette = match omarchy_palette {
        Ok(Some(palette)) => palette,
        Ok(None) => {
            return SystemResolution {
                target: fallback,
                warning: None,
            };
        }
        Err(reason) => {
            return SystemResolution {
                target: fallback,
                warning: Some(reason),
            };
        }
    };

    match omarchy_config(&palette) {
        Ok(config) => SystemResolution {
            target: ThemeTarget::Omarchy(config),
            warning: None,
        },
        Err(reason) => SystemResolution {
            target: fallback,
            warning: Some(reason),
        },
    }
}

/// Registers the bundled themes and shows the Qwerio theme matching the
/// system appearance. Call once after `gpui_kit::init`.
pub fn init(cx: &mut App) {
    for themes in [QWERIO_THEMES, NAMED_THEMES] {
        if let Err(error) = ThemeRegistry::global_mut(cx).load_themes_from_str(themes) {
            // The bundled files are part of the build; failing here is a bug.
            panic!("the bundled themes are invalid: {error}");
        }
    }

    let target = resolve_system(appearance_mode(cx.window_appearance()), Ok(None)).target;
    let config = config_for(&target, cx);

    // The first theme also sets the default text sizes.
    Theme::update(cx, |theme| install(theme, &config));
}

/// Shows `target` everywhere, see-through when `frosted_glass` is on. Text
/// sizes are kept, since people change them separately from the theme.
pub fn apply(target: &ThemeTarget, frosted_glass: FrostedGlass, cx: &mut App) {
    let config = config_for(target, cx);

    let opaque_background = Theme::update(cx, |theme| {
        let font_size = theme.font_size;
        let mono_font_size = theme.mono_font_size;

        install(theme, &config);
        let opaque_background = theme.background;
        if frosted_glass.is_on {
            glass::apply_frosted_glass(theme, frosted_glass.shade);
        }
        theme.font_size = font_size;
        theme.mono_font_size = mono_font_size;
        opaque_background
    });
    cx.set_global(OpaqueBackground(opaque_background));
}

fn config_for(target: &ThemeTarget, cx: &App) -> Rc<ThemeConfig> {
    match target {
        ThemeTarget::Omarchy(config) => Rc::clone(config),
        ThemeTarget::Bundled(name) => ThemeRegistry::global(cx)
            .themes()
            .get(*name)
            .cloned()
            // `init` registers every bundled name; a miss is a bug.
            .unwrap_or_else(|| panic!("the bundled theme \"{name}\" is not registered")),
    }
}

/// Installs `config` in its mode's slot and shows it. A theme without syntax
/// colors gets GPUI Kit's for its mode, so editors never keep the previous
/// theme's; their background then follows the theme's input background.
fn install(theme: &mut Theme, config: &Rc<ThemeConfig>) {
    theme.apply_config(config);

    if config.highlight.is_none() {
        theme.highlight_theme = fallback_highlight(config.mode);
    }
}

fn fallback_highlight(mode: ThemeMode) -> Arc<HighlightTheme> {
    let default = if mode.is_dark() {
        HighlightTheme::default_dark()
    } else {
        HighlightTheme::default_light()
    };
    let mut highlight = (*default).clone();
    highlight.style.editor_background = None;
    highlight.style.editor_gutter_background = None;

    Arc::new(highlight)
}

pub fn appearance_mode(appearance: WindowAppearance) -> ThemeMode {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
        WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
    }
}

pub fn omarchy_directory(custom: Option<PathBuf>) -> Option<PathBuf> {
    custom.or_else(omarchy::default_theme_directory)
}

/// Reads the active Omarchy palette. Blocking; call off the UI thread.
pub fn read_omarchy_palette(custom_directory: Option<PathBuf>) -> OmarchyPalette {
    let Some(directory) = omarchy_directory(custom_directory) else {
        return Ok(None);
    };
    let path = omarchy::palette_path(&directory);

    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn omarchy_config(palette: &str) -> Result<Rc<ThemeConfig>, String> {
    let json = omarchy::theme_set_json(palette).map_err(|error| error.to_string())?;
    let set: ThemeSet = serde_json::from_value(json).map_err(|error| error.to_string())?;

    set.themes
        .into_iter()
        .next()
        .map(Rc::new)
        .ok_or_else(|| "The Omarchy palette produced no theme".to_owned())
}

/// Watches the directory that holds Omarchy's active theme. Omarchy swaps the
/// whole theme directory on each switch, so the parent is watched, not the
/// palette file. `on_change` runs on the watcher's thread.
pub fn watch_omarchy(
    custom_directory: Option<PathBuf>,
    on_change: impl Fn() + Send + 'static,
) -> Result<RecommendedWatcher, String> {
    let directory = omarchy_directory(custom_directory).ok_or("No home directory")?;
    let parent = directory.parent().map(PathBuf::from).unwrap_or(directory);

    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok() {
            on_change();
        }
    })
    .map_err(|error| error.to_string())?;

    watcher
        .watch(&parent, RecursiveMode::Recursive)
        .map_err(|error| format!("{}: {error}", parent.display()))?;

    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OMARCHY_PALETTE: &str = r##"
        background = "#101010"
        foreground = "#f0f0f0"
        accent = "#ff8800"
    "##;

    fn bundled_name(resolution: &SystemResolution) -> Option<&'static str> {
        match resolution.target {
            ThemeTarget::Bundled(name) => Some(name),
            ThemeTarget::Omarchy(_) => None,
        }
    }

    fn bundled_modes() -> Vec<(String, ThemeMode)> {
        [QWERIO_THEMES, NAMED_THEMES]
            .into_iter()
            .flat_map(|text| {
                serde_json::from_str::<ThemeSet>(text)
                    .expect("bundled theme set")
                    .themes
            })
            .map(|theme| (theme.name.to_string(), theme.mode))
            .collect()
    }

    #[test]
    fn every_named_preference_is_bundled_in_its_mode() {
        let bundled = bundled_modes();
        let expected = LIGHT_THEMES
            .iter()
            .map(|preference| (*preference, ThemeMode::Light))
            .chain(
                DARK_THEMES
                    .iter()
                    .map(|preference| (*preference, ThemeMode::Dark)),
            );

        for (preference, mode) in expected {
            let name = preference.bundled_theme().expect("a named theme");
            assert!(
                bundled.contains(&(name.to_owned(), mode)),
                "{name} is not bundled as a {mode:?} theme"
            );
        }
    }

    #[test]
    fn named_themes_are_listed_by_name() {
        for themes in [&LIGHT_THEMES[..], &DARK_THEMES[..]] {
            let labels: Vec<&str> = themes.iter().map(|theme| theme.label()).collect();
            let mut sorted = labels.clone();
            sorted.sort_by_key(|label| label.to_lowercase());

            assert_eq!(labels, sorted);
        }
    }

    #[test]
    fn named_themes_bring_their_own_editor_colors() {
        let set: ThemeSet = serde_json::from_str(NAMED_THEMES).expect("named themes");

        for theme in set.themes {
            assert!(
                theme.highlight.is_some(),
                "{} has no syntax colors",
                theme.name
            );
        }
    }

    #[test]
    fn system_follows_the_appearance_without_omarchy() {
        let light = resolve_system(ThemeMode::Light, Ok(None));
        let dark = resolve_system(ThemeMode::Dark, Ok(None));

        assert_eq!(bundled_name(&light), Some(SYSTEM_LIGHT_THEME));
        assert_eq!(bundled_name(&dark), Some(SYSTEM_DARK_THEME));
        assert!(light.warning.is_none() && dark.warning.is_none());
    }

    #[test]
    fn system_uses_the_omarchy_palette_when_installed() {
        let resolution = resolve_system(ThemeMode::Light, Ok(Some(OMARCHY_PALETTE.to_owned())));

        let ThemeTarget::Omarchy(config) = resolution.target else {
            panic!("expected the Omarchy palette");
        };
        assert_eq!(config.mode, ThemeMode::Dark);
        assert!(resolution.warning.is_none());
    }

    #[test]
    fn an_unusable_omarchy_palette_falls_back_with_a_warning() {
        let invalid = resolve_system(ThemeMode::Dark, Ok(Some("not = [toml".to_owned())));
        let unreadable = resolve_system(ThemeMode::Light, Err("permission denied".to_owned()));

        assert_eq!(bundled_name(&invalid), Some(SYSTEM_DARK_THEME));
        assert!(invalid.warning.is_some());
        assert_eq!(bundled_name(&unreadable), Some(SYSTEM_LIGHT_THEME));
        assert_eq!(unreadable.warning.as_deref(), Some("permission denied"));
    }

    #[test]
    fn a_missing_omarchy_palette_means_omarchy_is_not_installed() {
        let directory = tempfile::TempDir::new().expect("temporary directory");

        let palette = read_omarchy_palette(Some(directory.path().join("theme")));

        assert_eq!(palette, Ok(None));
    }
}
