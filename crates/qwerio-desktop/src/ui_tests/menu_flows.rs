//! The toolbar menu keeps application commands available without a native menu bar.

use super::*;
use gpui_kit::test::TestWindowExt as _;

fn open_file_menu(cx: &mut TestAppContext) -> (Fixture, AnyWindowHandle) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    cx.update(crate::actions::set_menus);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the toolbar", |window, _| {
        window.try_find("application-menu").is_some()
    });
    interact(window, cx, |window, cx| {
        window.click("application-menu", cx)
    });
    wait_until(window, cx, "the application menu", |window, _| {
        window.try_find("popup-menu").is_some()
    });
    interact(window, cx, |window, cx| {
        let mut menu = window.within("popup-menu");
        assert_eq!(menu.find(1_usize).label(), Some("File"));
        menu.click(1_usize, cx);
    });
    interact(window, cx, |window, cx| window.press("right", cx));

    (fixture, window)
}

#[gpui_kit::test]
fn toolbar_menu_precedes_the_sidebar_and_escape_dismisses_it(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    cx.update(crate::actions::set_menus);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the toolbar", |window, _| {
        window.try_find("toggle-sidebar").is_some()
    });

    interact(window, cx, |window, cx| {
        let menu = window.find("application-menu").bounds();
        let sidebar = window.find("toggle-sidebar").bounds();
        assert!(menu.right() <= sidebar.left());
        window.click("application-menu", cx);
    });
    wait_until(window, cx, "the application menu", |window, _| {
        window.try_find("popup-menu").is_some()
    });
    interact(window, cx, |window, cx| window.press("escape", cx));
    wait_until(window, cx, "the menu dismissed", |window, _| {
        window.try_find("popup-menu").is_none()
    });
    interact(window, cx, |window, cx| window.click("new-note", cx));
    wait_until(window, cx, "toolbar commands still work", |window, _| {
        label_of(window, "status").is_some_and(|status| status.starts_with("Saved"))
    });
}

#[gpui_kit::test]
fn file_menu_creates_a_workspace(cx: &mut TestAppContext) {
    let (fixture, window) = open_file_menu(cx);
    interact(window, cx, |window, cx| window.press("enter", cx));
    super::workspace_files::wait_for_picker(cx, true);
    let path = fixture.root.with_file_name("From Menu.qwerio");
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    wait_until(window, cx, "encryption choice", |window, _| {
        window.try_find("workspace-without-password").is_some()
    });
    interact(window, cx, |window, cx| {
        window.click("workspace-without-password", cx)
    });
    wait_until(window, cx, "new workspace", |window, _| {
        label_of(window, "place-root").as_deref() == Some("From Menu")
    });
    assert!(Workspace::open_file(&path, None).is_ok());
}

#[gpui_kit::test]
fn file_menu_opens_a_workspace_and_cancel_keeps_it_open(cx: &mut TestAppContext) {
    let (fixture, window) = open_file_menu(cx);
    let path = fixture.root.with_file_name("Other.qwerio");
    Workspace::create_file(&path, "Other", None).expect("workspace to open");
    interact(window, cx, |window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    });
    super::workspace_files::wait_for_picker(cx, false);
    cx.simulate_path_prompt_response(|_| Some(vec![path]));
    wait_until(window, cx, "selected workspace", |window, _| {
        label_of(window, "place-root").as_deref() == Some("Other")
    });

    interact(window, cx, |window, cx| {
        window.click("application-menu", cx)
    });
    wait_until(window, cx, "the application menu", |window, _| {
        window.try_find("popup-menu").is_some()
    });
    interact(window, cx, |window, cx| {
        window.within("popup-menu").click(1_usize, cx)
    });
    interact(window, cx, |window, cx| {
        window.press("right", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    super::workspace_files::wait_for_picker(cx, false);
    cx.simulate_path_prompt_response(|_| None);
    interact(window, cx, |window, _| {
        assert_eq!(label_of(window, "place-root").as_deref(), Some("Other"));
    });
}
