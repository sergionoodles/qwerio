//! Opening workspaces and moving around the explorer.

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;

use qwerio_workspace::Workspace;
use tempfile::TempDir;

use super::*;

#[gpui_kit::test]
fn reopens_the_last_workspace_on_start(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    fixture.record_recent();
    init_app(fixture.services.clone(), cx);

    let (window, _) = open_workbench(None, cx);

    let connection = fixture.connection_key.clone();
    let note = fixture.note_key.clone();
    wait_until(
        window,
        cx,
        "the explorer lists the workspace",
        |window, _| {
            window.try_find(connection.clone()).is_some() && window.try_find(note.clone()).is_some()
        },
    );
    interact(window, cx, |window, _| {
        assert_eq!(
            window.find(connection.clone()).label(),
            Some(CONNECTION_NAME)
        );
        assert_eq!(window.find(note.clone()).label(), Some("readme.md"));
        assert!(window.try_find("open-workspace").is_none());
    });
}

#[gpui_kit::test]
fn shows_the_welcome_screen_before_the_first_workspace(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);

    let (window, _) = open_workbench(None, cx);

    wait_until(window, cx, "the welcome screen shows", |window, _| {
        window.try_find("open-workspace").is_some() && window.try_find("new-workspace").is_some()
    });
}

#[gpui_kit::test]
fn opens_another_workspace_without_a_selector(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("Other.qwerio");
    Workspace::create_file(&path, "Other", None).expect("workspace");
    init_app(fixture.services.clone(), cx);
    let (window, workbench) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "first workspace", |window, _| {
        window.try_find(fixture.connection_key.clone()).is_some()
    });
    interact(window, cx, |window, cx| {
        workbench.update(cx, |workbench, cx| {
            workbench.open_path(path.clone(), None, window, cx)
        })
    });
    wait_until(window, cx, "other workspace", |window, _| {
        label_of(window, "place-root").as_deref() == Some("Other")
    });
    interact(window, cx, |window, _| {
        assert!(window.try_find("switch-workspace").is_none());
        assert!(window.try_find(fixture.connection_key.clone()).is_none());
    });
}

#[gpui_kit::test]
fn browses_a_table_with_the_keyboard(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);

    let connection = fixture.connection_key.clone();
    wait_until(window, cx, "the connection is listed", |window, _| {
        window.try_find(connection.clone()).is_some()
    });

    interact(window, cx, |window, cx| {
        window.click(connection.clone(), cx);
        assert_eq!(window.find(connection.clone()).selected(), Some(true));
        window.press("enter", cx);
    });
    wait_until(
        window,
        cx,
        "the connection's tables are listed",
        |window, _| label_of(window, "status").as_deref() == Some("2 items"),
    );

    interact(window, cx, |window, cx| {
        window.press("home", cx);
        window.press("enter", cx);
    });
    wait_until(window, cx, "the first table's rows load", |window, _| {
        label_of(window, "status").as_deref() == Some("3 rows")
    });
    interact(window, cx, |window, _| {
        assert_eq!(window.find("cell-0-1").label(), Some("Ada"));
        assert_eq!(window.find("cell-2-1").label(), Some("Linus"));
    });

    // The first click sorts descending; the server returns re-sorted rows.
    interact(window, cx, |window, cx| {
        window.click(("icon-sort", 1_usize), cx)
    });
    wait_until(window, cx, "rows sort by name descending", |window, _| {
        label_of(window, "cell-0-1").as_deref() == Some("Linus")
    });
    interact(window, cx, |window, _| {
        assert_eq!(window.find("cell-2-1").label(), Some("Ada"));
    });
}

#[gpui_kit::test]
fn navigates_back_and_forward_and_splits_the_view(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);

    let connection = fixture.connection_key.clone();
    wait_until(window, cx, "the connection is listed", |window, _| {
        window.try_find(connection.clone()).is_some()
    });
    interact(window, cx, |window, cx| {
        window.double_click(connection.clone(), cx)
    });
    wait_until(window, cx, "the connection opens", |window, _| {
        label_of(window, "status").as_deref() == Some("2 items")
            && window.try_find(connection.clone()).is_none()
    });

    interact(window, cx, |window, cx| window.click("back", cx));
    wait_until(window, cx, "back returns to the workspace", |window, _| {
        window.try_find(connection.clone()).is_some()
    });

    interact(window, cx, |window, cx| window.click("forward", cx));
    wait_until(
        window,
        cx,
        "forward returns to the connection",
        |window, _| window.try_find(connection.clone()).is_none(),
    );

    interact(window, cx, |window, cx| window.click("split", cx));
    wait_until(
        window,
        cx,
        "a split opens in one shared tab",
        |window, _| {
            window.try_find("tab-bar").is_some()
                && window.try_find("close-tab-0").is_some()
                && window.try_find("close-tab-1").is_none()
        },
    );
    interact(window, cx, |window, cx| window.click("split", cx));
    wait_until(window, cx, "the split closes", |window, _| {
        window.try_find("tab-bar").is_none()
    });
}

#[gpui_kit::test]
fn first_run_creates_a_plain_default_after_the_users_choice(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let path = fixture
        .services
        .local_state
        .setting::<std::path::PathBuf>("default_workspace_path")
        .expect("setting")
        .expect("path");
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(None, cx);
    wait_until(window, cx, "first-run password choice", |window, _| {
        window.try_find("workspace-without-password").is_some()
    });
    assert!(
        !path.exists(),
        "do not write plaintext before the encryption choice"
    );
    interact(window, cx, |window, cx| {
        window.click("workspace-without-password", cx)
    });
    wait_until(window, cx, "default workspace", |window, _| {
        label_of(window, "place-root").as_deref() == Some("Default")
    });
    assert!(
        !Workspace::open_file(&path, None)
            .expect("default file")
            .is_encrypted()
    );
}

#[gpui_kit::test]
fn first_run_can_encrypt_the_default_and_unlock_it_on_reopen(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let path = fixture
        .services
        .local_state
        .setting::<std::path::PathBuf>("default_workspace_path")
        .expect("setting")
        .expect("path");
    init_app(fixture.services.clone(), cx);
    let (window, workbench) = open_workbench(None, cx);
    wait_until(window, cx, "first-run choice", |window, _| {
        window.try_find("workspace-password").is_some()
    });
    interact(window, cx, |window, cx| {
        window.click("workspace-password", cx);
        window.input("workspace secret", cx);
        window.click("workspace-password-confirm", cx);
        window.input("workspace secret", cx);
        window.click("workspace-password-accept", cx);
    });
    wait_until(window, cx, "encrypted default", |window, _| {
        label_of(window, "place-root").as_deref() == Some("Default")
    });
    assert!(Workspace::open_file(&path, None).is_err());
    interact(window, cx, |window, cx| {
        window.dispatch_action(Box::new(crate::actions::CloseWorkspace), cx)
    });
    wait_until(window, cx, "workspace closed", |window, _| {
        window.try_find("open-workspace").is_some()
    });
    interact(window, cx, |window, cx| {
        workbench.update(cx, |workbench, cx| {
            workbench.open_path(path.clone(), None, window, cx)
        })
    });
    wait_until(window, cx, "unlock", |window, _| {
        window.try_find("workspace-password").is_some()
    });
    interact(window, cx, |window, cx| {
        assert!(window.try_find("workspace-without-password").is_none());
        window.click("workspace-password", cx);
        window.input("workspace secret", cx);
        window.click("workspace-password-accept", cx);
    });
    wait_until(window, cx, "unlocked", |window, _| {
        window.try_find("workspace-password").is_none()
            && label_of(window, "place-root").as_deref() == Some("Default")
    });
}

#[gpui_kit::test]
fn shows_the_tab_bar_only_with_more_than_one_tab(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the workspace opens", |window, _| {
        window.try_find(fixture.connection_key.clone()).is_some()
    });
    interact(window, cx, |window, _| {
        assert!(window.try_find("tab-bar").is_none());
    });

    interact(window, cx, |window, cx| window.click("new-tab", cx));
    wait_until(window, cx, "the tab bar shows", |window, _| {
        window.try_find("tab-bar").is_some()
    });

    interact(window, cx, |window, cx| {
        window.dispatch_action(Box::new(crate::actions::CloseTab), cx)
    });
    wait_until(window, cx, "the tab bar hides", |window, _| {
        window.try_find("tab-bar").is_none()
    });
}

#[gpui_kit::test]
fn hides_the_first_tab_border_at_the_window_edge_without_a_sidebar(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the workspace opens", |window, _| {
        window.try_find(fixture.connection_key.clone()).is_some()
    });
    interact(window, cx, |window, cx| window.click("new-tab", cx));
    wait_until(window, cx, "the tab bar", |window, _| {
        window.try_find("tab-bar").is_some()
    });
    interact(window, cx, |window, _| {
        assert!(window.find("tab-bar").bounds().left() > px(0.));
    });

    interact(window, cx, |window, cx| window.click("toggle-sidebar", cx));

    wait_until(
        window,
        cx,
        "the tab bar pushed past the window edge",
        |window, _| window.find("tab-bar").bounds().left() < px(0.),
    );
}

/// The handle between the sidebar and the content: where the sidebar ends.
const SIDEBAR_HANDLE: (&str, usize) = ("resizable-handle", 0);

#[gpui_kit::test]
fn the_sidebar_keeps_its_width_when_the_window_resizes(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the sidebar shows", |window, _| {
        window
            .within("shell-sidebar")
            .try_find(SIDEBAR_HANDLE)
            .is_some()
    });
    let sidebar_end = |window: &mut Window| {
        window
            .within("shell-sidebar")
            .find(SIDEBAR_HANDLE)
            .bounds()
            .origin
            .x
    };
    let before = interact(window, cx, |window, _| sidebar_end(window));

    for width in [px(1600.), px(960.)] {
        cx.simulate_window_resize(window, size(width, px(800.)));
        interact(window, cx, |window, _| {
            assert_eq!(sidebar_end(window), before, "in a {width:?} wide window");
        });
    }
}
