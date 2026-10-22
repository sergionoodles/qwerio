//! Creating tables and columns from the toolbar.

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;

use super::*;

/// Opens the shop connection, and its first table when `open_table` is set.
pub(super) fn open_connection(
    fixture: &Fixture,
    open_table: bool,
    cx: &mut TestAppContext,
) -> AnyWindowHandle {
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);

    let connection = fixture.connection_key.clone();
    wait_until(window, cx, "the connection is listed", |window, _| {
        window.try_find(connection.clone()).is_some()
    });
    interact(window, cx, |window, cx| {
        window.double_click(connection.clone(), cx)
    });
    wait_until(
        window,
        cx,
        "the connection's tables are listed",
        |window, _| label_of(window, "status").as_deref() == Some("2 items"),
    );

    if open_table {
        interact(window, cx, |window, cx| {
            window.press("home", cx);
            window.press("enter", cx);
        });
        wait_until(window, cx, "the customers table opens", |window, _| {
            label_of(window, "status").as_deref() == Some("3 rows")
        });
    }

    window
}

fn preview(window: &Window) -> String {
    label_of(window, "schema-preview")
        .map(|label| label.to_string())
        .unwrap_or_default()
}

fn column_names(fixture: &Fixture, table: &str) -> Vec<String> {
    let connection = rusqlite::Connection::open(&fixture.database).expect("open database");
    let mut statement = connection
        .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .expect("prepare");
    statement
        .query_map([table], |row| row.get(0))
        .expect("query columns")
        .collect::<Result<_, _>>()
        .expect("column names")
}

#[gpui_kit::test]
fn creates_a_table_from_the_toolbar_and_opens_it(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_connection(&fixture, false, cx);

    interact(window, cx, |window, cx| window.click("new-table", cx));
    wait_until(window, cx, "the table form opens", |window, _| {
        window.try_find("schema-name").is_some()
    });
    interact(window, cx, |window, _| {
        assert_eq!(preview(window), "Enter a name for the table");
    });

    interact(window, cx, |window, cx| {
        window.input("invoices", cx);
        window.click("add-column-row", cx);
        window.input("total", cx);
    });
    wait_until(
        window,
        cx,
        "the preview shows the statement",
        |window, _| preview(window).contains("\"total\" TEXT"),
    );
    interact(window, cx, |window, cx| {
        assert_eq!(
            preview(window),
            "CREATE TABLE \"invoices\" (\n    \"id\" INTEGER NOT NULL PRIMARY KEY,\n    \"total\" TEXT\n)"
        );
        window.click("schema-create", cx);
    });

    wait_until(window, cx, "the new table opens", |window, _| {
        window.try_find("schema-name").is_none()
            && label_of(window, "status").as_deref() == Some("0 rows")
    });
    assert_eq!(column_names(&fixture, "invoices"), ["id", "total"]);
}

#[gpui_kit::test]
fn adds_a_column_to_the_open_table(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_connection(&fixture, true, cx);

    interact(window, cx, |window, cx| window.click("new-column", cx));
    wait_until(window, cx, "the column form opens", |window, _| {
        window.try_find("column-name").is_some()
    });
    interact(window, cx, |window, cx| {
        window.input("email", cx);
        window.click("column-default", cx);
        window.input("'none'", cx);
        window.click("schema-create", cx);
    });

    wait_until(window, cx, "the form closes", |window, _| {
        window.try_find("column-name").is_none()
    });
    assert_eq!(column_names(&fixture, "customers"), ["id", "name", "email"]);
}

#[gpui_kit::test]
fn keeps_the_form_open_when_the_database_rejects_the_change(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_connection(&fixture, true, cx);

    interact(window, cx, |window, cx| window.click("new-column", cx));
    wait_until(window, cx, "the column form opens", |window, _| {
        window.try_find("column-name").is_some()
    });
    // SQLite cannot add a NOT NULL column without a default to existing rows.
    interact(window, cx, |window, cx| {
        window.input("email", cx);
        window.click("column-nullable", cx);
        window.click("schema-create", cx);
    });

    wait_until(window, cx, "the error is shown", |window, _| {
        window.try_find("schema-error").is_some()
    });
    interact(window, cx, |window, _| {
        assert!(window.try_find("column-name").is_some());
        let error = label_of(window, "schema-error").unwrap_or_default();
        assert!(error.contains("NOT NULL"), "unexpected error: {error}");
    });
    assert_eq!(column_names(&fixture, "customers"), ["id", "name"]);
}
