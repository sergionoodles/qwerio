//! Context menus belong to the entry under the pointer, or to empty space;
//! containers of tables can show their relationships as a graph.

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    InputEvent as _, Modifiers, MouseMoveEvent, Pixels, ScrollDelta, ScrollWheelEvent, TouchPhase,
    point,
};

use super::*;
use crate::explorer_view::node_widths;
use crate::schema_graph::{EdgeEnds, layout_graph};

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
        interact(window, cx, |window, cx| window.click("layout-icons", cx));
    }

    for (key, command) in [
        (fixture.connection_key.clone(), "Edit Connection…"),
        (fixture.note_key.clone(), "Rename…"),
    ] {
        interact(window, cx, |window, cx| window.right_click(key, cx));
        wait_until(window, cx, "one entry menu opens", |window, _| {
            window.try_find("popup-menu").is_some()
        });
        interact(window, cx, |window, cx| {
            let menu = window.within("popup-menu");
            assert_eq!(menu.find(0_usize).label(), Some("Open"));
            assert_eq!(menu.find(2_usize).label(), Some("Labels…"));
            assert_eq!(menu.find(4_usize).label(), Some(command));
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

/// Opens the fixture's SQLite database and switches to the graph.
fn open_graph(fixture: &Fixture, cx: &mut TestAppContext) -> AnyWindowHandle {
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the connection is listed", |window, _| {
        window.try_find(fixture.connection_key.clone()).is_some()
    });

    interact(window, cx, |window, cx| {
        window.double_click(fixture.connection_key.clone(), cx)
    });
    wait_until(window, cx, "the tables are listed", |window, _| {
        label_of(window, "status").as_deref() == Some("2 items")
    });

    interact(window, cx, |window, cx| window.click("layout-graph", cx));
    wait_until(
        window,
        cx,
        "the graph shows one relationship",
        |window, _| label_of(window, "status").as_deref() == Some("2 items · 1 relationship"),
    );

    window
}

#[gpui_kit::test]
fn shows_relationships_as_a_graph_and_describes_hovered_lines(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_graph(&fixture, cx);

    // Tables list by name, so orders (1) references customers (0).
    let widths = cx.update(|cx| node_widths(&["customers", "orders"], cx));
    let layout = layout_graph(
        &widths,
        &[EdgeEnds {
            referencing: 1,
            referenced: 0,
        }],
    );
    let middle = layout.routes[0].midpoint().expect("the edge is routed");

    interact(window, cx, |window, cx| {
        let origin = window.find("explorer-graph-content").bounds().origin;
        move_pointer(window, origin + point(px(middle.x), px(middle.y)), cx);
    });
    wait_until(window, cx, "hovering the line describes it", |window, _| {
        label_of(window, "relationship-details").as_deref() == Some("orders references customers")
    });
}

#[gpui_kit::test]
fn zooms_the_graph_with_keys_wheel_and_buttons(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_graph(&fixture, cx);
    let zoom_label = |window: &Window| label_of(window, "graph-zoom-reset");

    // Pressing empty space focuses the graph, so its zoom keys apply
    // instead of the workbench's text size keys.
    interact(window, cx, |window, cx| {
        assert_eq!(zoom_label(window).as_deref(), Some("100%"));
        window.click_at("explorer-graph-content", point(px(4.), px(4.)), cx);
        window.press("ctrl-=", cx);
    });
    wait_until(window, cx, "Ctrl and + zooms in", |window, _| {
        zoom_label(window).as_deref() == Some("120%")
    });

    interact(window, cx, |window, cx| {
        let position = window.find("explorer-graph-content").bounds().center();
        window.dispatch_event(
            ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(0.), px(-60.))),
                modifiers: Modifiers::control(),
                touch_phase: TouchPhase::Moved,
            }
            .to_platform_input(),
            cx,
        );
    });
    wait_until(window, cx, "Ctrl and the wheel zooms out", |window, _| {
        zoom_label(window).as_deref() == Some("100%")
    });

    interact(window, cx, |window, cx| window.click("graph-zoom-out", cx));
    wait_until(window, cx, "the zoom out button zooms out", |window, _| {
        zoom_label(window).as_deref() == Some("83%")
    });
}

fn move_pointer(window: &mut Window, position: Point<Pixels>, cx: &mut App) {
    window.dispatch_event(
        MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Default::default(),
        }
        .to_platform_input(),
        cx,
    );
}
