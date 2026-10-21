//! Adding and editing connections through the connection dialog.

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;
use qwerio_workspace::Workspace;

use super::*;

/// Left empty, the name defaults to the engine and the database file.
const NEW_CONNECTION_NAME: &str = "sqlite@shop.sqlite";
/// The engine list starts on PostgreSQL; SQLite is next.
const POSTGRES_TO_SQLITE_STEPS: usize = 1;

fn assert_footer_alignment(window: AnyWindowHandle, cx: &mut TestAppContext) {
    interact(window, cx, |window, _| {
        let name = window.find("connection-name").bounds();
        let cancel = window.find("cancel-connection").bounds();
        let test = window.find("test-connection").bounds();
        let save = window.find("save-connection").bounds();

        assert_eq!(cancel.origin.x, name.origin.x);
        assert!(cancel.size.width < name.size.width / 2.);
        assert!(test.origin.x > name.center().x);
        assert!(test.right() < save.origin.x);
        assert_eq!(save.right(), name.right());
    });
}

fn open_sidebar_connection_settings(fixture: &Fixture, cx: &mut TestAppContext) -> AnyWindowHandle {
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    let connection = Workspace::open(&fixture.root)
        .expect("workspace")
        .manifest()
        .connections[0]
        .id;
    let sidebar_key = SharedString::from(format!("connection-{connection}"));
    wait_until(
        window,
        cx,
        "the sidebar connection is listed",
        |window, _| window.try_find(sidebar_key.clone()).is_some(),
    );
    interact(window, cx, |window, cx| window.right_click(sidebar_key, cx));
    wait_until(window, cx, "the connection menu opens", |window, _| {
        window.try_find("popup-menu").is_some()
    });
    interact(window, cx, |window, cx| {
        let mut menu = window.within("popup-menu");
        assert_eq!(menu.find(0_usize).label(), Some("Edit Connection…"));
        menu.click(0_usize, cx);
    });
    let database = fixture.database.display().to_string();
    wait_until(window, cx, "the saved settings load", |window, _| {
        window
            .try_find("connection-file")
            .is_some_and(|field| field.value() == Some(database.as_str()))
    });
    interact(window, cx, |window, _| {
        assert_eq!(
            window.find("connection-name").value(),
            Some(CONNECTION_NAME)
        );
        assert!(window.try_find("connection-engine").is_none());
    });
    window
}

#[gpui_kit::test]
fn edits_a_connection_from_the_sidebar(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_sidebar_connection_settings(&fixture, cx);
    assert_footer_alignment(window, cx);
    let original = Workspace::open(&fixture.root).expect("workspace");
    let connection = original.manifest().connections[0].id;
    let replacement = fixture.database.with_file_name("replacement.sqlite");
    seed_database(&replacement);
    const EDITED_NAME: &str = "Updated shop";

    interact(window, cx, |window, cx| {
        window.click("connection-name", cx);
        window.press("ctrl-a", cx);
        window.input(EDITED_NAME, cx);
        window.click("connection-file", cx);
        window.press("ctrl-a", cx);
        window.input(&replacement.display().to_string(), cx);
        window.click("foreign-keys", cx);
        window.click("save-connection", cx);
    });
    wait_until(window, cx, "the editor closes after saving", |window, _| {
        window.try_find("connection-name").is_none()
    });
    wait_until(window, cx, "the connection name updates", |window, _| {
        label_of(window, fixture.connection_key.clone()).as_deref() == Some(EDITED_NAME)
    });

    let workspace = Workspace::open(&fixture.root).expect("workspace");
    assert_eq!(workspace.manifest().connections.len(), 1);
    assert_eq!(workspace.manifest().connections[0].id, connection);
    assert_eq!(workspace.manifest().connections[0].name, EDITED_NAME);
    let resolved = fixture
        .services
        .resolve_connection(workspace.manifest(), connection)
        .expect("saved connection");
    let ConnectionOptions::Sqlite(options) = resolved.descriptor.options else {
        panic!("the connection must stay SQLite");
    };
    assert_eq!(options.path, replacement);
    assert!(!options.enforce_foreign_keys);
}

#[gpui_kit::test]
fn validates_connection_edits_and_discards_them_on_cancel(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_sidebar_connection_settings(&fixture, cx);
    interact(window, cx, |window, cx| {
        window.click("connection-name", cx);
        window.press("ctrl-a", cx);
        window.press("backspace", cx);
        window.click("save-connection", cx);
    });
    wait_until(window, cx, "an empty name is rejected", |window, _| {
        label_of(window, "connection-status").as_deref() == Some("Enter a name for the connection")
    });
    interact(window, cx, |window, cx| {
        window.click("connection-name", cx);
        window.input("Discard this name", cx);
        window.click("cancel-connection", cx);
    });
    wait_until(window, cx, "Cancel closes the editor", |window, _| {
        window.try_find("connection-name").is_none()
    });
    let workspace = Workspace::open(&fixture.root).expect("workspace");
    assert_eq!(workspace.manifest().connections.len(), 1);
    assert_eq!(workspace.manifest().connections[0].name, CONNECTION_NAME);
    let resolved = fixture
        .services
        .resolve_connection(workspace.manifest(), workspace.manifest().connections[0].id)
        .expect("unchanged connection");
    let ConnectionOptions::Sqlite(options) = resolved.descriptor.options else {
        panic!("the connection must stay SQLite");
    };
    assert_eq!(options.path, fixture.database);
}

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
    assert_footer_alignment(window, cx);
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

#[gpui_kit::test]
fn fills_the_form_from_a_pasted_connection_string(cx: &mut TestAppContext) {
    const INVALID_URL: &str = "mysql://db/shop";
    const POSTGRES_URL: &str = "DATABASE_URL=postgres://app:s3cret@db.example.com:6543/shop";
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the workspace root is listed", |window, _| {
        label_of(window, "status").as_deref() == Some("2 items")
    });

    interact(window, cx, |window, cx| window.click("add-connection", cx));
    wait_until(window, cx, "the dialog opens", |window, _| {
        window.try_find("connection-string").is_some()
    });
    choose_option(window, cx, "connection-engine", POSTGRES_TO_SQLITE_STEPS);
    interact(window, cx, |window, cx| {
        window.click("connection-string", cx);
        window.input(INVALID_URL, cx);
    });
    wait_until(
        window,
        cx,
        "an unsupported scheme is reported",
        |window, _| {
            label_of(window, "connection-string-feedback")
                .is_some_and(|label| label.contains("not supported"))
        },
    );

    interact(window, cx, |window, cx| {
        window.press("ctrl-a", cx);
        window.input(POSTGRES_URL, cx);
    });
    wait_until(
        window,
        cx,
        "the PostgreSQL fields are filled",
        |window, _| {
            window
                .try_find("connection-host")
                .is_some_and(|field| field.value() == Some("db.example.com"))
        },
    );
    interact(window, cx, |window, _| {
        assert_eq!(window.find("connection-port").value(), Some("6543"));
        assert_eq!(window.find("connection-user").value(), Some("app"));
        assert_eq!(window.find("connection-database").value(), Some("shop"));
    });

    let sqlite_url = format!("sqlite://{}", fixture.database.display());
    interact(window, cx, |window, cx| {
        window.click("connection-string", cx);
        window.press("ctrl-a", cx);
        window.input(&sqlite_url, cx);
    });
    let database = fixture.database.display().to_string();
    wait_until(window, cx, "the SQLite file is filled", |window, _| {
        window
            .try_find("connection-file")
            .is_some_and(|field| field.value() == Some(database.as_str()))
    });
    interact(window, cx, |window, cx| window.click("save-connection", cx));
    wait_until(window, cx, "the dialog closes", |window, _| {
        window.try_find("connection-name").is_none()
    });

    let workspace = Workspace::open(&fixture.root).expect("workspace");
    let added = workspace
        .manifest()
        .connections
        .iter()
        .find(|connection| connection.name == NEW_CONNECTION_NAME)
        .expect("the manifest lists the new connection");
    let resolved = fixture
        .services
        .resolve_connection(workspace.manifest(), added.id)
        .expect("saved connection");
    let ConnectionOptions::Sqlite(options) = resolved.descriptor.options else {
        panic!("the connection must be SQLite");
    };
    assert_eq!(options.path, fixture.database);
}
