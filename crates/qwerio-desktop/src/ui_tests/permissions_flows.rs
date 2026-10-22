//! Native manager interactions without requiring a live server. Engine behavior
//! is covered separately against disposable PostgreSQL, MySQL, and MariaDB.

use gpui_kit::test::TestWindowExt as _;

use super::*;

fn open_manager(fixture: &Fixture, engine: EngineKind, cx: &mut TestAppContext) -> AnyWindowHandle {
    let mut workspace = Workspace::open(&fixture.root).expect("workspace");
    let connection = workspace
        .add_connection("Administration", engine)
        .expect("alias");
    let key = SharedString::from(format!("connection-{connection}"));
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the server appears", |window, _| {
        window.try_find(key.clone()).is_some()
    });
    interact(window, cx, |window, cx| window.right_click(key, cx));
    wait_until(window, cx, "the menu opens", |window, _| {
        window.try_find("popup-menu").is_some()
    });
    interact(window, cx, |window, cx| {
        let mut menu = window.within("popup-menu");
        assert_eq!(menu.find(1_usize).label(), Some("Users & permissions…"));
        menu.click(1_usize, cx);
    });
    wait_until(window, cx, "unbound connection is reported", |window, _| {
        window.try_find("permission-error").is_some()
    });
    window
}

#[gpui_kit::test]
fn permissions_manager_previews_postgres_changes(cx: &mut TestAppContext) {
    check_manager(EngineKind::Postgres, cx);
}

#[gpui_kit::test]
fn permissions_manager_previews_mysql_changes(cx: &mut TestAppContext) {
    check_manager(EngineKind::Mysql, cx);
}

#[gpui_kit::test]
fn permissions_manager_previews_mariadb_changes(cx: &mut TestAppContext) {
    check_manager(EngineKind::Mariadb, cx);
}

fn check_manager(engine: EngineKind, cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_manager(&fixture, engine, cx);
    interact(window, cx, |window, cx| {
        let summary = label_of(window, "permission-pending-summary").expect("pending summary");
        assert!(summary.starts_with("No pending changes"));
        window.click("permission-new-user", cx);
    });
    wait_until(window, cx, "the new user form opens", |window, _| {
        window.try_find("permission-name").is_some()
    });
    interact(window, cx, |window, cx| {
        assert_eq!(
            window.try_find("permission-host").is_some(),
            engine != EngineKind::Postgres
        );
        window.click("permission-name", cx);
        window.input(" alice ", cx);
        window.click("permission-password", cx);
        window.input("fake-preview-password", cx);
    });
    wait_until(window, cx, "the redacted preview updates", |window, _| {
        label_of(window, "permission-create-preview").is_some_and(|preview| {
            preview.starts_with("CREATE USER ") && preview.contains("[redacted]")
        })
    });
    interact(window, cx, |window, cx| {
        let preview = label_of(window, "permission-create-preview").expect("preview");
        assert!(!preview.contains("fake-preview-password"));
        assert!(
            preview.contains(" alice "),
            "Account names must retain their exact spelling"
        );
        window.click("permission-create", cx);
    });
    wait_until(
        window,
        cx,
        "failed creation leaves the form usable",
        |window, _| {
            window.try_find("permission-error").is_some()
                && window.try_find("permission-name").is_some()
        },
    );
    interact(window, cx, |window, cx| {
        window.click("permission-new-role", cx)
    });
    wait_until(
        window,
        cx,
        "role creation replaces the user form",
        |window, _| window.try_find("permission-password").is_none(),
    );
    interact(window, cx, |window, cx| {
        assert_eq!(
            window.try_find("permission-host").is_some(),
            engine == EngineKind::Mysql
        );
        window.click("permission-name", cx);
        window.input("readers", cx);
    });
    wait_until(window, cx, "the role preview updates", |window, _| {
        label_of(window, "permission-create-preview").is_some_and(|preview| {
            preview.starts_with("CREATE ROLE ") && preview.contains("readers")
        })
    });
}

#[gpui_kit::test]
fn permissions_are_disabled_for_sqlite(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = super::schema_flows::open_connection(&fixture, false, cx);
    interact(window, cx, |window, cx| {
        window.click("open-permissions", cx);
        assert!(window.try_find("permissions-manager").is_none());
    });
}
