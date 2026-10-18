//! A versioned adapter that maps the active Omarchy palette into a GPUI Kit
//! theme. It only reads color data; theme scripts are never executed.
//!
//! Omarchy stages the active theme under `~/.local/state/omarchy/current/theme`
//! and replaces that directory when switching, so watchers observe its parent.
//! Two palette shapes are understood: the named keys of `colors.toml`
//! (`background`, `accent`, `red`, …) and the older terminal-style keys
//! (`color0`…`color15`, `selection_background`).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

pub const PALETTE_FILE: &str = "colors.toml";

/// The theme name used for the generated configuration.
pub const OMARCHY_THEME_NAME: &str = "Omarchy";

/// `~/.local/state/omarchy/current/theme`, Omarchy's active theme directory.
pub fn default_theme_directory() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|directories| {
        directories
            .home_dir()
            .join(".local/state/omarchy/current/theme")
    })
}

pub fn palette_path(theme_directory: &Path) -> PathBuf {
    theme_directory.join(PALETTE_FILE)
}

#[derive(Debug, PartialEq, Eq)]
pub enum PaletteError {
    Invalid(String),
    MissingColor(&'static str),
}

impl std::fmt::Display for PaletteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => {
                write!(formatter, "The Omarchy palette is invalid: {message}")
            }
            Self::MissingColor(key) => {
                write!(formatter, "The Omarchy palette has no \"{key}\" color")
            }
        }
    }
}

/// Colors resolved from either palette shape.
struct Palette {
    is_dark: bool,
    background: String,
    foreground: String,
    accent: String,
    muted: String,
    panel: String,
    border: String,
    selection: String,
    red: String,
    green: String,
    yellow: String,
    blue: String,
    cyan: String,
    magenta: String,
}

/// Builds a GPUI Kit theme set (JSON) from the text of `colors.toml`.
pub fn theme_set_json(palette_toml: &str) -> Result<Value, PaletteError> {
    let table: toml::Table =
        toml::from_str(palette_toml).map_err(|error| PaletteError::Invalid(error.to_string()))?;
    let palette = resolve(&table)?;
    let mode = if palette.is_dark { "dark" } else { "light" };

    let mut colors = Map::new();
    let mut set = |key: &str, value: &str| {
        colors.insert(key.to_owned(), Value::String(value.to_owned()));
    };

    set("background", &palette.background);
    set("foreground", &palette.foreground);
    set("border", &palette.border);
    set("window.border", &palette.border);
    set("input.border", &palette.border);
    set("ring", &palette.accent);
    set("caret", &palette.foreground);
    set("muted.background", &palette.panel);
    set("muted.foreground", &palette.muted);
    set("accent.background", &palette.panel);
    set("accent.foreground", &palette.foreground);
    set("primary.background", &palette.accent);
    set("primary.foreground", &palette.background);
    set("secondary.background", &palette.panel);
    set("secondary.foreground", &palette.foreground);
    set("selection.background", &format!("{}66", palette.selection));
    set("popover.background", &palette.panel);
    set("popover.foreground", &palette.foreground);
    set("sidebar.background", &palette.panel);
    set("sidebar.foreground", &palette.foreground);
    set("sidebar.border", &palette.border);
    set("sidebar.accent.background", &palette.border);
    set("sidebar.accent.foreground", &palette.foreground);
    set("sidebar.primary.background", &palette.accent);
    set("sidebar.primary.foreground", &palette.background);
    set("list.active.background", &format!("{}33", palette.accent));
    set("list.active.border", &palette.accent);
    set("list.hover.background", &palette.panel);
    set("table.head.background", &palette.panel);
    set("table.head.foreground", &palette.muted);
    set("table.active.background", &format!("{}33", palette.accent));
    set("table.active.border", &palette.accent);
    set("table.hover.background", &palette.panel);
    set("tab_bar.background", &palette.panel);
    set("tab.active.background", &palette.background);
    set("tab.active.foreground", &palette.foreground);
    set("tab.foreground", &palette.muted);
    set("title_bar.background", &palette.panel);
    set("status_bar.background", &palette.panel);
    set("scrollbar.thumb.background", &palette.border);
    set("danger.background", &palette.red);
    set("danger.foreground", &palette.background);
    set("warning.background", &palette.yellow);
    set("warning.foreground", &palette.background);
    set("success.background", &palette.green);
    set("success.foreground", &palette.background);
    set("info.background", &palette.blue);
    set("info.foreground", &palette.background);
    set("base.red", &palette.red);
    set("base.green", &palette.green);
    set("base.yellow", &palette.yellow);
    set("base.blue", &palette.blue);
    set("base.cyan", &palette.cyan);
    set("base.magenta", &palette.magenta);

    Ok(json!({
        "name": OMARCHY_THEME_NAME,
        "themes": [{
            "name": OMARCHY_THEME_NAME,
            "mode": mode,
            "colors": colors,
        }],
    }))
}

fn resolve(table: &toml::Table) -> Result<Palette, PaletteError> {
    let color = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            table
                .get(*key)
                .and_then(toml::Value::as_str)
                .and_then(normalize_hex)
        })
    };
    let required =
        |name: &'static str, keys: &[&str]| color(keys).ok_or(PaletteError::MissingColor(name));

    let background = required("background", &["background", "color0"])?;
    let foreground = required("foreground", &["foreground", "color7", "color15"])?;
    let is_dark = match table.get("mode").and_then(toml::Value::as_str) {
        Some(mode) => mode != "light",
        None => luminance(&background) < luminance(&foreground),
    };

    let accent = color(&["accent", "color4", "cursor"]).unwrap_or_else(|| foreground.clone());
    let panel = color(&["dark_background", "lighter_background", "color0"])
        .unwrap_or_else(|| background.clone());
    let border = color(&["lighter_background", "color8"]).unwrap_or_else(|| panel.clone());

    Ok(Palette {
        is_dark,
        muted: color(&["muted", "dark_foreground", "color8"]).unwrap_or_else(|| foreground.clone()),
        selection: color(&["selection", "selection_background"]).unwrap_or_else(|| accent.clone()),
        red: color(&["red", "color1"]).unwrap_or_else(|| accent.clone()),
        green: color(&["green", "color2"]).unwrap_or_else(|| accent.clone()),
        yellow: color(&["yellow", "color3"]).unwrap_or_else(|| accent.clone()),
        blue: color(&["blue", "color4"]).unwrap_or_else(|| accent.clone()),
        magenta: color(&["magenta", "color5"]).unwrap_or_else(|| accent.clone()),
        cyan: color(&["cyan", "color6"]).unwrap_or_else(|| accent.clone()),
        background,
        foreground,
        accent,
        panel,
        border,
    })
}

/// Accepts `#rgb`, `#rrggbb`, and `0xrrggbb`; returns `#rrggbb` or `None`.
fn normalize_hex(text: &str) -> Option<String> {
    let digits = text
        .trim()
        .strip_prefix('#')
        .or_else(|| text.trim().strip_prefix("0x"))?;

    if !digits
        .chars()
        .all(|character| character.is_ascii_hexdigit())
    {
        return None;
    }

    match digits.len() {
        6 => Some(format!("#{}", digits.to_ascii_lowercase())),
        3 => Some(format!(
            "#{}",
            digits
                .chars()
                .flat_map(|character| [character, character])
                .collect::<String>()
                .to_ascii_lowercase()
        )),
        _ => None,
    }
}

/// Relative luminance used only to guess the mode when it is not declared.
fn luminance(hex: &str) -> f32 {
    let channel = |range: std::ops::Range<usize>| {
        u8::from_str_radix(&hex[range], 16).map_or(0.0, |value| f32::from(value) / 255.0)
    };

    0.2126 * channel(1..3) + 0.7152 * channel(3..5) + 0.0722 * channel(5..7)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors(json: &Value) -> &Map<String, Value> {
        json["themes"][0]["colors"].as_object().expect("colors")
    }

    #[test]
    fn named_palettes_map_to_semantic_roles() {
        let palette = r##"
            mode = "dark"
            accent = "#7aa2f7"
            background = "#1a1b26"
            foreground = "#a9b1d6"
            dark_background = "#16161e"
            lighter_background = "#292e42"
            muted = "#565f89"
            selection = "#283457"
            red = "#f7768e"
            green = "#9ece6a"
            yellow = "#e0af68"
            blue = "#7aa2f7"
        "##;

        let json = theme_set_json(palette).expect("theme");
        let colors = colors(&json);

        assert_eq!(json["themes"][0]["mode"], "dark");
        assert_eq!(colors["primary.background"], "#7aa2f7");
        assert_eq!(colors["danger.background"], "#f7768e");
        assert_eq!(colors["sidebar.background"], "#16161e");
        assert_eq!(colors["selection.background"], "#28345766");
    }

    #[test]
    fn terminal_style_palettes_are_supported_and_mode_is_inferred() {
        let palette = r##"
            color0 = "#fafafa"
            color1 = "#c00"
            color4 = "#0050a0"
            color7 = "#202020"
        "##;

        let json = theme_set_json(palette).expect("theme");

        assert_eq!(json["themes"][0]["mode"], "light");
        assert_eq!(colors(&json)["danger.background"], "#cc0000");
        assert_eq!(colors(&json)["primary.background"], "#0050a0");
    }

    #[test]
    fn invalid_or_incomplete_palettes_are_rejected() {
        assert!(matches!(
            theme_set_json("not = [toml"),
            Err(PaletteError::Invalid(_))
        ));
        assert_eq!(
            theme_set_json("foreground = \"#ffffff\""),
            Err(PaletteError::MissingColor("background"))
        );
    }

    #[test]
    fn color_values_that_are_not_hex_are_ignored() {
        let palette = r##"
            background = "#000000"
            foreground = "#ffffff"
            accent = "$(rm -rf ~)"
        "##;

        let json = theme_set_json(palette).expect("theme");

        assert_eq!(colors(&json)["primary.background"], "#ffffff");
    }
}
