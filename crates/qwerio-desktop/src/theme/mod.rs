//! Theme sources: the system appearance, explicit light or dark, or the
//! active Omarchy palette. Each resolves to a GPUI Kit theme configuration;
//! components only read semantic tokens from it.

pub mod omarchy;

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry, ThemeSet};
use gpui_kit::{App, SharedString, Window, WindowAppearance};
use serde::{Deserialize, Serialize};

const QWERIO_THEMES: &str = include_str!("../../assets/themes/qwerio.json");
const LIGHT_THEME: &str = "Qwerio Light";
const DARK_THEME: &str = "Qwerio Dark";

/// Settings key for the chosen source.
pub const THEME_SOURCE_SETTING: &str = "theme_source";

/// Settings key for a custom Omarchy theme directory.
pub const OMARCHY_DIRECTORY_SETTING: &str = "omarchy_theme_directory";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeSource {
    #[default]
    System,
    Light,
    Dark,
    Omarchy,
}

/// Registers the Qwerio themes. Call once after `gpui_kit::init`.
pub fn init(cx: &mut App) {
    if let Err(error) = ThemeRegistry::global_mut(cx).load_themes_from_str(QWERIO_THEMES) {
        // The bundled file is part of the build; failing here is a bug.
        panic!("the bundled Qwerio themes are invalid: {error}");
    }

    restore_bundled_themes(cx);
}

/// The outcome of applying a source; Omarchy falls back to the system
/// appearance when its palette is missing or invalid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    Source(ThemeSource),
    FellBack { reason: String },
}

/// Applies a source. `omarchy_palette` is the palette text read off the UI
/// thread with [`read_omarchy_palette`]; it is only used for Omarchy.
pub fn apply(
    source: ThemeSource,
    omarchy_palette: Option<Result<String, String>>,
    window: &mut Window,
    cx: &mut App,
) -> Applied {
    restore_bundled_themes(cx);

    match source {
        ThemeSource::System => {
            Theme::change(appearance_mode(window.appearance()), None, cx);
            Applied::Source(source)
        }
        ThemeSource::Light => {
            Theme::change(ThemeMode::Light, None, cx);
            Applied::Source(source)
        }
        ThemeSource::Dark => {
            Theme::change(ThemeMode::Dark, None, cx);
            Applied::Source(source)
        }
        ThemeSource::Omarchy => match omarchy_config(omarchy_palette) {
            Ok(config) => {
                let mode = config.mode;
                Theme::update(cx, |theme| theme.apply_config(&config));
                Theme::change(mode, None, cx);
                Applied::Source(source)
            }
            Err(reason) => {
                Theme::change(appearance_mode(window.appearance()), None, cx);
                Applied::FellBack { reason }
            }
        },
    }
}

fn restore_bundled_themes(cx: &mut App) {
    let themes = ThemeRegistry::global(cx).themes();
    let light = themes.get(&SharedString::from(LIGHT_THEME)).cloned();
    let dark = themes.get(&SharedString::from(DARK_THEME)).cloned();
    let theme = Theme::global_mut(cx);

    if let Some(light) = light {
        theme.light_theme = light;
    }

    if let Some(dark) = dark {
        theme.dark_theme = dark;
    }
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
pub fn read_omarchy_palette(custom_directory: Option<PathBuf>) -> Result<String, String> {
    let directory = omarchy_directory(custom_directory).ok_or("No home directory")?;
    let path = omarchy::palette_path(&directory);
    std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))
}

fn omarchy_config(palette: Option<Result<String, String>>) -> Result<Rc<ThemeConfig>, String> {
    let text = palette.ok_or("The Omarchy palette was not loaded")??;
    let json = omarchy::theme_set_json(&text).map_err(|error| error.to_string())?;
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
