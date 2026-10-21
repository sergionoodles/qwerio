//! The Settings dialog and theme preferences.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{TestAppContext, point};
use tempfile::TempDir;

use super::*;
use crate::theme::glass::{FROSTED_GLASS_SETTING, FROSTED_GLASS_SHADE_SETTING, MAX_SHADE};
use crate::theme::omarchy::{OMARCHY_THEME_NAME, PALETTE_FILE};
use crate::theme::{
    DARK_THEMES, LIGHT_THEMES, OMARCHY_DIRECTORY_SETTING, THEME_SETTING, ThemePreference,
};
use crate::workbench::FONT_SIZE_SETTING;

/// The first dark theme in the picker, and the steps from System down to it:
/// every light theme, then it.
const FIRST_DARK_THEME: ThemePreference = DARK_THEMES[0];
const STEPS_TO_FIRST_DARK_THEME: usize = LIGHT_THEMES.len() + 1;
const SAVED_FONT_SIZE: f32 = 18.;

fn save_setting(fixture: &Fixture, key: &str, value: &impl serde::Serialize) {
    fixture
        .services
        .local_state
        .set_setting(key, value)
        .expect("save setting");
}

fn open_workspace(fixture: &Fixture, cx: &mut TestAppContext) -> AnyWindowHandle {
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the toolbar shows", |window, _| {
        window.try_find("settings").is_some()
    });
    window
}

fn open_settings(window: AnyWindowHandle, cx: &mut TestAppContext) {
    interact(window, cx, |window, cx| window.click("settings", cx));
    wait_until(window, cx, "the Settings dialog opens", |window, _| {
        window.try_find("settings-theme").is_some()
    });
}

fn wait_for_theme(window: AnyWindowHandle, cx: &mut TestAppContext, name: &str) {
    wait_until(window, cx, &format!("{name} is shown"), |_, cx| {
        cx.theme().theme_name().as_ref() == name
    });
}

/// A stand-in Omarchy install: `theme/colors.toml` inside a temporary folder.
fn write_omarchy_palette(root: &TempDir, background: &str) {
    let theme = root.path().join("theme");
    std::fs::create_dir_all(&theme).expect("theme folder");
    std::fs::write(
        theme.join(PALETTE_FILE),
        format!("background = \"{background}\"\nforeground = \"#f0f0f0\"\naccent = \"#ff8800\"\n"),
    )
    .expect("palette");
}

#[gpui_kit::test]
fn choosing_a_theme_applies_and_saves_it(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_workspace(&fixture, cx);
    open_settings(window, cx);

    interact(window, cx, |window, _| {
        assert_eq!(
            label_of(window, "settings-section-title").as_deref(),
            Some("Appearance")
        );
        assert_eq!(window.find("settings-theme").value(), Some("System"));
    });

    choose_option(window, cx, "settings-theme", STEPS_TO_FIRST_DARK_THEME);
    wait_for_theme(window, cx, FIRST_DARK_THEME.label());

    let saved: Option<ThemePreference> = fixture
        .services
        .local_state
        .setting(THEME_SETTING)
        .expect("read setting");
    assert_eq!(saved, Some(FIRST_DARK_THEME));

    interact(window, cx, |window, cx| {
        assert!(cx.theme().is_dark());
        window.click("settings-close", cx);
    });
    wait_until(window, cx, "the Settings dialog closes", |window, _| {
        window.try_find("settings-theme").is_none()
    });

    open_settings(window, cx);
    interact(window, cx, |window, _| {
        assert_eq!(
            window.find("settings-theme").value(),
            Some(FIRST_DARK_THEME.label())
        );
    });
}

#[gpui_kit::test]
fn the_theme_row_keeps_its_description_beside_the_picker(cx: &mut TestAppContext) {
    // The description once collapsed to one character per line, stretching
    // the dialog down the whole window.
    const MAX_ROW_HEIGHT: f32 = 120.;
    const MIN_ROW_WIDTH: f32 = 400.;

    let fixture = Fixture::new();
    let window = open_workspace(&fixture, cx);
    open_settings(window, cx);

    interact(window, cx, |window, _| {
        let row = window.find("settings-theme-row").bounds();
        let picker = window.find("settings-theme").bounds();
        let done = window.find("settings-close").bounds();

        assert!(row.size.height < px(MAX_ROW_HEIGHT), "row is {row:?}");
        assert!(row.size.width > px(MIN_ROW_WIDTH), "row is {row:?}");
        assert!(picker.right() <= row.right());
        assert!(done.size.width < row.size.width / 2.);
    });
}

#[gpui_kit::test]
fn the_saved_theme_returns_on_start(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    save_setting(&fixture, THEME_SETTING, &ThemePreference::GruvboxDark);

    let window = open_workspace(&fixture, cx);

    wait_for_theme(window, cx, "Gruvbox Dark");
}

#[gpui_kit::test]
fn changing_the_theme_keeps_the_text_size(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    save_setting(&fixture, FONT_SIZE_SETTING, &SAVED_FONT_SIZE);
    let window = open_workspace(&fixture, cx);
    wait_until(window, cx, "the saved text size is shown", |_, cx| {
        cx.theme().font_size == px(SAVED_FONT_SIZE)
    });

    open_settings(window, cx);
    choose_option(window, cx, "settings-theme", STEPS_TO_FIRST_DARK_THEME);
    wait_for_theme(window, cx, FIRST_DARK_THEME.label());

    interact(window, cx, |_, cx| {
        assert_eq!(cx.theme().font_size, px(SAVED_FONT_SIZE));
    });
}

#[gpui_kit::test]
fn a_theme_save_failure_is_visible_and_a_later_save_can_recover(cx: &mut TestAppContext) {
    let mut fixture = Fixture::new();
    let path = fixture.root.join("preferences.sqlite");
    let local_state = LocalState::open(&path).expect("preferences");
    let mut drivers = DriverRegistry::new();
    drivers.register(Arc::new(SqliteDriver::new()));
    fixture.services = Arc::new(AppServices::new(
        drivers,
        local_state,
        Arc::new(MemoryCredentialStore::default()),
    ));
    let connection = rusqlite::Connection::open(&path).expect("test connection");
    connection.execute_batch("CREATE TRIGGER reject_settings BEFORE INSERT ON settings BEGIN SELECT RAISE(ABORT, 'test preference write failure'); END;").expect("reject preference writes");
    let window = open_workspace(&fixture, cx);
    open_settings(window, cx);
    choose_option(window, cx, "settings-theme", STEPS_TO_FIRST_DARK_THEME);
    wait_for_theme(window, cx, FIRST_DARK_THEME.label());
    wait_until(window, cx, "theme save error", |window, _| {
        window.try_find("settings-theme-error").is_some()
    });
    connection
        .execute_batch("DROP TRIGGER reject_settings;")
        .expect("restore preference writes");
    choose_option(window, cx, "settings-theme", 1);
    wait_for_theme(window, cx, "Catppuccin Mocha");
    wait_until(window, cx, "theme save recovered", |window, _| {
        window.try_find("settings-theme-error").is_none()
    });
    assert_eq!(
        fixture
            .services
            .local_state
            .setting::<ThemePreference>(THEME_SETTING)
            .expect("preference"),
        Some(ThemePreference::CatppuccinMocha)
    );
}

#[gpui_kit::test]
fn system_follows_omarchy_theme_changes(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let omarchy = TempDir::new().expect("temporary directory");
    write_omarchy_palette(&omarchy, "#101010");
    save_setting(
        &fixture,
        OMARCHY_DIRECTORY_SETTING,
        &omarchy.path().join("theme"),
    );

    let window = open_workspace(&fixture, cx);
    wait_for_theme(window, cx, OMARCHY_THEME_NAME);
    let first_background = interact(window, cx, |_, cx| cx.theme().background);

    write_omarchy_palette(&omarchy, "#202838");
    wait_until(window, cx, "the new Omarchy palette is shown", |_, cx| {
        cx.theme().background != first_background
    });
}

#[gpui_kit::test]
fn adding_a_global_label_does_not_copy_private_workspace_definitions(cx: &mut TestAppContext) {
    use qwerio_core::connection::Secret;
    let fixture = Fixture::new();
    let path = fixture.root.with_file_name("private.qwerio");
    let password = Secret::new("workspace password");
    let mut workspace =
        Workspace::create_file(&path, "Private", Some(&password)).expect("workspace");
    workspace
        .define_label("workspace-only secret label")
        .expect("private label");
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(path), cx);
    wait_until(window, cx, "unlock dialog", |window, _| {
        window.try_find("workspace-password").is_some()
    });
    interact(window, cx, |window, cx| {
        window.click("workspace-password", cx);
        window.input(password.expose(), cx);
        window.click("workspace-password-accept", cx);
    });
    wait_until(window, cx, "toolbar", |window, _| {
        window.try_find("settings").is_some()
    });
    open_settings(window, cx);
    interact(window, cx, |window, cx| {
        window.click("settings-section-labels", cx);
        window.click("new-label-name", cx);
        window.input("shared label", cx);
    });
    // Render the section after its preference load before saving the new label.
    interact(window, cx, |window, cx| window.click("add-label", cx));
    wait_until(window, cx, "global label saved", |_, _| {
        fixture
            .services
            .local_state
            .setting::<Vec<String>>(crate::settings_dialog::labels::LABELS_SETTING)
            .expect("setting")
            .is_some_and(|labels| labels.contains(&"shared label".into()))
    });
    let labels = fixture
        .services
        .local_state
        .setting::<Vec<String>>(crate::settings_dialog::labels::LABELS_SETTING)
        .expect("setting")
        .expect("saved labels");
    assert!(!labels.contains(&"workspace-only secret label".into()));
}

fn is_see_through(cx: &App) -> bool {
    cx.theme().background.a < 1.
}

fn saved_frosted_glass(fixture: &Fixture) -> Option<bool> {
    fixture
        .services
        .local_state
        .setting(FROSTED_GLASS_SETTING)
        .expect("read setting")
}

#[gpui_kit::test]
fn frosted_glass_is_off_until_turned_on_and_is_saved(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_workspace(&fixture, cx);
    open_settings(window, cx);
    let opaque_background = interact(window, cx, |_, cx| cx.theme().background);
    assert_eq!(opaque_background.a, 1.);

    interact(window, cx, |window, cx| {
        window.click("settings-frosted-glass", cx)
    });
    wait_until(window, cx, "the window turns see-through", |_, cx| {
        is_see_through(cx)
    });
    assert_eq!(saved_frosted_glass(&fixture), Some(true));
    interact(window, cx, |_, cx| {
        let background = cx.theme().background;
        assert_eq!(
            (background.h, background.s),
            (opaque_background.h, opaque_background.s)
        );
    });

    interact(window, cx, |window, cx| {
        window.click("settings-frosted-glass", cx)
    });
    wait_until(window, cx, "the window turns opaque", |_, cx| {
        cx.theme().background == opaque_background
    });
    assert_eq!(saved_frosted_glass(&fixture), Some(false));
}

#[gpui_kit::test]
fn saved_frosted_glass_applies_on_start_and_follows_theme_changes(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    save_setting(&fixture, FROSTED_GLASS_SETTING, &true);

    let window = open_workspace(&fixture, cx);
    wait_until(window, cx, "the saved frosted glass is shown", |_, cx| {
        is_see_through(cx)
    });

    open_settings(window, cx);
    choose_option(window, cx, "settings-theme", STEPS_TO_FIRST_DARK_THEME);
    wait_for_theme(window, cx, FIRST_DARK_THEME.label());
    interact(window, cx, |_, cx| assert!(is_see_through(cx)));
}

#[gpui_kit::test]
fn the_shade_slider_shows_with_a_transparent_background_and_is_saved(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    save_setting(&fixture, THEME_SETTING, &FIRST_DARK_THEME);
    let window = open_workspace(&fixture, cx);
    wait_for_theme(window, cx, FIRST_DARK_THEME.label());
    open_settings(window, cx);
    interact(window, cx, |window, _| {
        assert!(window.try_find("settings-frosted-glass-shade").is_none());
    });

    interact(window, cx, |window, cx| {
        window.click("settings-frosted-glass", cx)
    });
    wait_until(window, cx, "the shade slider shows", |window, _| {
        window.try_find("settings-frosted-glass-shade").is_some()
    });
    let default_background = interact(window, cx, |_, cx| cx.theme().background);

    interact(window, cx, |window, cx| {
        let size = window.find("settings-frosted-glass-shade").bounds().size;
        let far_end = point(size.width - px(1.), size.height / 2.);
        window.click_at("settings-frosted-glass-shade", far_end, cx);
    });
    wait_until(window, cx, "the background darkens", |_, cx| {
        cx.theme().background.l < default_background.l
    });
    let saved_shade: Option<f32> = fixture
        .services
        .local_state
        .setting(FROSTED_GLASS_SHADE_SETTING)
        .expect("read setting");
    assert!(saved_shade.is_some_and(|shade| shade > MAX_SHADE * 0.9));

    interact(window, cx, |window, cx| {
        window.click("settings-frosted-glass", cx)
    });
    wait_until(window, cx, "the shade slider hides", |window, _| {
        window.try_find("settings-frosted-glass-shade").is_none()
    });
}
