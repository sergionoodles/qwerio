//! Adding and editing connections through the connection dialog.

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;
use qwerio_workspace::Workspace;

use super::*;

/// Left empty, the name defaults to the engine and the database file.
const NEW_CONNECTION_NAME: &str = "sqlite@shop.sqlite";

/// `leading` is the footer's left button: Cancel when editing, Back in the wizard.
fn assert_footer_alignment(
    window: AnyWindowHandle,
    cx: &mut TestAppContext,
    leading: &'static str,
) {
    interact(window, cx, |window, _| {
        let name = window.find("connection-name").bounds();
        let cancel = window.find(leading).bounds();
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
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let workspace = Workspace::open(&fixture.root).expect("workspace");
    let connection = workspace.manifest().connections[0].id;
    let resolved = fixture
        .services
        .resolve_connection(workspace.manifest(), connection)
        .expect("resolve");
    runtime
        .block_on(fixture.services.sessions.browse(
            &resolved.descriptor,
            &resolved.credentials,
            None,
        ))
        .expect("cache browsing session");
    let window = open_sidebar_connection_settings(&fixture, cx);
    assert_footer_alignment(window, cx, "cancel-connection");
    let original = Workspace::open(&fixture.root).expect("workspace");
    let connection = original.manifest().connections[0].id;
    let replacement = fixture.database.with_file_name("replacement.sqlite");
    seed_database(&replacement);
    rusqlite::Connection::open(&replacement)
        .expect("replacement database")
        .execute_batch("CREATE TABLE replacement_only (id INTEGER PRIMARY KEY)")
        .expect("replacement catalog");
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
    let ConnectionOptions::Sqlite(options) = &resolved.descriptor.options else {
        panic!("the connection must stay SQLite");
    };
    assert_eq!(options.path, replacement);
    assert!(!options.enforce_foreign_keys);
    let entries = runtime.block_on(async {
        let session = fixture
            .services
            .sessions
            .browse(&resolved.descriptor, &resolved.credentials, None)
            .await
            .expect("updated session");
        session
            .list(&qwerio_core::resource::CatalogPath::root())
            .await
            .expect("updated catalog")
    });
    assert!(entries.iter().any(|entry| entry.name == "replacement_only"));
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

    start_new_connection(window, cx, "engine-sqlite");
    assert_footer_alignment(window, cx, "connection-back");

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
    start_new_connection(window, cx, "engine-postgres");

    interact(window, cx, |window, cx| {
        assert!(window.try_find("connection-string").is_none());
        window.click("use-connection-string", cx);
    });
    wait_until(
        window,
        cx,
        "the connection string replaces the fields",
        |window, _| {
            window.try_find("connection-string").is_some()
                && window.try_find("connection-host").is_none()
        },
    );
    interact(window, cx, |window, cx| window.click("save-connection", cx));
    wait_until(window, cx, "an empty string is rejected", |window, _| {
        label_of(window, "connection-status")
            .is_some_and(|label| label.starts_with("Paste a connection string"))
    });

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
    wait_until(window, cx, "the string is read", |window, _| {
        label_of(window, "connection-string-feedback")
            .is_some_and(|label| label.starts_with("Details filled in"))
    });
    interact(window, cx, |window, cx| {
        window.click("use-connection-string", cx)
    });
    wait_until(window, cx, "the filled fields return", |window, _| {
        window
            .try_find("connection-host")
            .is_some_and(|field| field.value() == Some("db.example.com"))
    });
    interact(window, cx, |window, cx| {
        assert_eq!(window.find("connection-port").value(), Some("6543"));
        assert_eq!(window.find("connection-user").value(), Some("app"));
        assert_eq!(window.find("connection-database").value(), Some("shop"));
        window.click("connection-back", cx);
    });

    wait_until(window, cx, "the engine step returns", |window, _| {
        window.try_find("engine-sqlite").is_some()
    });
    interact(window, cx, |window, cx| {
        window.click("engine-sqlite", cx);
        window.click("connection-continue", cx);
    });
    wait_until(window, cx, "SQLite is chosen", |window, _| {
        window.try_find("connection-file").is_some()
    });
    interact(window, cx, |window, _| {
        assert!(window.try_find("use-connection-string").is_none());
        assert!(window.try_find("connection-string").is_none());
        assert!(window.try_find("show-all-databases").is_none());
    });
}

fn adds_server_connection(engine: EngineKind, engine_id: &'static str, cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "workspace loads", |window, _| {
        label_of(window, "status").as_deref() == Some("2 items")
    });
    start_new_connection(window, cx, engine_id);
    interact(window, cx, |window, cx| {
        let default_port = if engine == EngineKind::Postgres {
            "5432"
        } else {
            "3306"
        };
        assert_eq!(window.find("connection-port").value(), Some(default_port));
        assert_eq!(window.find("show-all-databases").checked(), Some(true));
        window.click("show-all-databases", cx);
        window.click("use-connection-string", cx);
    });
    wait_until(window, cx, "URL field appears", |window, _| {
        window.try_find("connection-string").is_some()
    });
    interact(window, cx, |window, cx| {
        window.click("connection-string", cx);
        let url = if engine == EngineKind::Postgres {
            "postgres://app:%20private%20password%20@localhost:3307/shop"
        } else {
            "mysql://app:%20private%20password%20@localhost:3307/shop"
        };
        window.input(url, cx);
    });
    wait_until(window, cx, "URL applies", |window, _| {
        label_of(window, "connection-string-feedback")
            .is_some_and(|text| text.starts_with("Details filled in"))
    });
    interact(window, cx, |window, cx| {
        assert_eq!(window.find("show-all-databases").checked(), Some(false));
        window.click("use-connection-string", cx)
    });
    wait_until(window, cx, "fields return", |window, _| {
        window.try_find("connection-host").is_some()
    });
    interact(window, cx, |window, cx| {
        assert_eq!(window.find("connection-host").value(), Some("localhost"));
        assert_eq!(window.find("connection-port").value(), Some("3307"));
        assert_eq!(window.find("connection-user").value(), Some("app"));
        assert_eq!(window.find("connection-database").value(), Some("shop"));
        window.click("save-connection", cx);
    });
    wait_until(window, cx, "connection saves", |window, _| {
        window.try_find("connection-name").is_none()
    });
    let workspace = Workspace::open(&fixture.root).expect("workspace");
    let alias = workspace
        .manifest()
        .connections
        .iter()
        .find(|alias| alias.engine == engine)
        .expect("server connection");
    let resolved = fixture
        .services
        .resolve_connection(workspace.manifest(), alias.id)
        .expect("saved endpoint");
    assert_eq!(resolved.descriptor.engine(), engine);
    let (host, port, database, show_all_databases) = match resolved.descriptor.options {
        ConnectionOptions::Postgres(options) => (
            options.host,
            options.port,
            options.database,
            options.show_all_databases,
        ),
        ConnectionOptions::Mysql(options) | ConnectionOptions::Mariadb(options) => (
            options.host,
            options.port,
            options.database,
            options.show_all_databases,
        ),
        ConnectionOptions::Sqlite(_) => panic!("server options"),
    };
    assert_eq!(host, "localhost");
    assert_eq!(port, 3307);
    assert_eq!(database, "shop");
    assert!(!show_all_databases);
    assert_eq!(
        resolved
            .credentials
            .password
            .as_ref()
            .map(|secret| secret.expose()),
        Some(" private password ")
    );
    reopens_database_visibility_setting(window, alias.id, cx);
}

fn reopens_database_visibility_setting(
    window: AnyWindowHandle,
    connection: qwerio_core::ids::ConnectionId,
    cx: &mut TestAppContext,
) {
    let sidebar_key = SharedString::from(format!("connection-{connection}"));
    wait_until(window, cx, "server appears in sidebar", |window, _| {
        window.try_find(sidebar_key.clone()).is_some()
    });
    interact(window, cx, |window, cx| window.right_click(sidebar_key, cx));
    wait_until(window, cx, "connection menu opens", |window, _| {
        window.try_find("popup-menu").is_some()
    });
    interact(window, cx, |window, cx| {
        window.within("popup-menu").click(0_usize, cx)
    });
    wait_until(window, cx, "saved visibility loads", |window, _| {
        window
            .try_find("connection-database")
            .is_some_and(|field| field.value() == Some("shop"))
    });
    interact(window, cx, |window, cx| {
        assert_eq!(window.find("show-all-databases").checked(), Some(false));
        window.click("cancel-connection", cx);
    });
}

#[gpui_kit::test]
fn adds_a_postgres_connection_with_url_and_persists_it(cx: &mut TestAppContext) {
    adds_server_connection(EngineKind::Postgres, "engine-postgres", cx);
}

#[gpui_kit::test]
fn adds_a_mysql_connection_with_url_and_persists_it(cx: &mut TestAppContext) {
    adds_server_connection(EngineKind::Mysql, "engine-mysql", cx);
}

#[gpui_kit::test]
fn adds_a_mariadb_connection_with_url_and_persists_it(cx: &mut TestAppContext) {
    adds_server_connection(EngineKind::Mariadb, "engine-mariadb", cx);
}
