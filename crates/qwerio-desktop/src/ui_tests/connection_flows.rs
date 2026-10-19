//! Adding a connection through the connection dialog.

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;
use qwerio_workspace::Workspace;

use super::*;

/// Left empty, the name defaults to the engine and the database file.
const NEW_CONNECTION_NAME: &str = "sqlite@shop.sqlite";
/// The engine list starts on PostgreSQL; SQLite is next.
const POSTGRES_TO_SQLITE_STEPS: usize = 1;

#[gpui_kit::test]
fn adds_a_sqlite_connection_through_the_dialog(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the workspace root is listed", |window, _| {
        label_of(window, "status").as_deref() == Some("2 items")
    });

    interact(window, cx, |window, cx| window.click("add-connection", cx));
    wait_until(window, cx, "the dialog opens", |window, _| {
        window.try_find("connection-engine").is_some()
    });
    choose_option(window, cx, "connection-engine", POSTGRES_TO_SQLITE_STEPS);
    wait_until(window, cx, "SQLite is chosen", |window, _| {
        window.try_find("connection-file").is_some()
    });

    let database = fixture.database.display().to_string();
    interact(window, cx, |window, cx| {
        window.click("connection-file", cx);
        window.input(&database, cx);
        window.click("save-connection", cx);
    });
    wait_until(window, cx, "the dialog closes", |window, _| {
        window.try_find("connection-name").is_none()
    });

    let manifest = Workspace::open(&fixture.root)
        .expect("workspace")
        .manifest()
        .clone();
    let added = manifest
        .connections
        .iter()
        .find(|connection| connection.name == NEW_CONNECTION_NAME)
        .expect("the manifest lists the new connection");
    let key = SharedString::from(format!("connection:{}", added.id));
    wait_until(
        window,
        cx,
        "the explorer lists the new connection",
        |window, _| label_of(window, key.clone()).as_deref() == Some(NEW_CONNECTION_NAME),
    );
}
