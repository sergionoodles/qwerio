//! The About dialog, opened from the toolbar.

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;

use super::*;
use crate::about_dialog::{APP_NAME, REPOSITORY_URL, VERSION};

fn open_about(cx: &mut TestAppContext) -> (Fixture, AnyWindowHandle) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the toolbar shows", |window, _| {
        window.try_find("about").is_some()
    });

    interact(window, cx, |window, cx| window.click("about", cx));
    wait_until(window, cx, "the About dialog opens", |window, _| {
        window.try_find("about-name").is_some()
    });

    (fixture, window)
}

#[gpui_kit::test]
fn shows_the_name_version_and_repository(cx: &mut TestAppContext) {
    let (_fixture, window) = open_about(cx);

    interact(window, cx, |window, _| {
        assert_eq!(label_of(window, "about-name").as_deref(), Some(APP_NAME));
        assert_eq!(
            label_of(window, "about-version"),
            Some(format!("Version: {VERSION}").into())
        );
        assert!(window.try_find("about-logo").is_some());
    });

    interact(window, cx, |window, cx| {
        window.click("about-repository", cx)
    });
    assert_eq!(cx.opened_url().as_deref(), Some(REPOSITORY_URL));
}

#[gpui_kit::test]
fn escape_dismisses_the_dialog(cx: &mut TestAppContext) {
    let (_fixture, window) = open_about(cx);

    interact(window, cx, |window, cx| window.press("escape", cx));
    wait_until(window, cx, "the About dialog closes", |window, _| {
        window.try_find("about-name").is_none()
    });
}

#[gpui_kit::test]
fn close_button_dismisses_the_dialog(cx: &mut TestAppContext) {
    let (_fixture, window) = open_about(cx);

    interact(window, cx, |window, cx| window.click("about-close", cx));
    wait_until(window, cx, "the About dialog closes", |window, _| {
        window.try_find("about-name").is_none()
    });
}
