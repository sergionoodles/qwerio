//! Context menus belong to the entry under the pointer, or to empty space.

use gpui_kit::test::TestWindowExt as _;

use super::*;

#[gpui_kit::test]
fn opens_one_entry_context_menu_in_details(cx: &mut TestAppContext) {
    check_entry_context_menus(false, cx);
}

#[gpui_kit::test]
fn opens_one_entry_context_menu_in_icons(cx: &mut TestAppContext) {
    check_entry_context_menus(true, cx);
}

fn check_entry_context_menus(is_icons: bool, cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the connection is listed", |window, _| {
        window.try_find(fixture.connection_key.clone()).is_some()
    });

    if is_icons {
        interact(window, cx, |window, cx| window.click("toggle-layout", cx));
    }

    for (key, command) in [
        (fixture.connection_key.clone(), "Connection Settings…"),
        (fixture.note_key.clone(), "Rename…"),
    ] {
        interact(window, cx, |window, cx| window.right_click(key, cx));
        wait_until(window, cx, "one entry menu opens", |window, _| {
            window.try_find("popup-menu").is_some()
        });
        interact(window, cx, |window, cx| {
            let menu = window.within("popup-menu");
            assert_eq!(menu.find(0_usize).label(), Some("Open"));
            assert_eq!(menu.find(3_usize).label(), Some(command));
            window.press("escape", cx);
        });
        wait_until(window, cx, "Escape dismisses the menu", |window, _| {
            window.try_find("popup-menu").is_none()
        });
    }

    interact(window, cx, |window, cx| window.right_click("explorer", cx));
    wait_until(window, cx, "the background menu opens", |window, _| {
        window.try_find("popup-menu").is_some()
    });
    interact(window, cx, |window, cx| {
        let mut menu = window.within("popup-menu");
        assert_eq!(menu.find(0_usize).label(), Some("New SQL Document"));
        assert_eq!(menu.find(1_usize).label(), Some("New Note"));
        menu.click(1_usize, cx);
    });
    wait_until(window, cx, "New Note opens a saved note", |window, _| {
        label_of(window, "status").is_some_and(|status| status.starts_with("Saved"))
    });
    let workspace = Workspace::open(&fixture.root).expect("workspace");
    assert_eq!(workspace.manifest().documents.len(), 2);
}
