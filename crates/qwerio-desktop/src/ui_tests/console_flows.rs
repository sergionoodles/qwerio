//! Running statements from a temporary query.

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;

use super::*;

const TARGET_LABEL: &str = "Shop data (SQLite)";
/// At the workspace root the New menu's database items are disabled: the
/// first press selects the top item, the second skips to the temporary query.
const TEMPORARY_QUERY_STEPS: usize = 2;

/// Opens a temporary query, types `sql` into its editor (focused when the
/// query opens), chooses the shop connection and `access`, and runs it with
/// the keyboard shortcut.
fn run_in_new_query(
    fixture: &Fixture,
    sql: &str,
    access: &'static str,
    cx: &mut TestAppContext,
) -> AnyWindowHandle {
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the workspace opens", |window, _| {
        window.try_find("new-menu").is_some()
    });

    choose_menu_item(window, cx, "new-menu", TEMPORARY_QUERY_STEPS);
    wait_until(window, cx, "the query opens", |window, _| {
        window.try_find("console-target").is_some()
    });
    interact(window, cx, |window, cx| window.input(sql, cx));

    choose_option(window, cx, "console-target", 1);
    wait_until(window, cx, "the target is chosen", |window, _| {
        window.find("console-target").value() == Some(TARGET_LABEL)
    });
    interact(window, cx, |window, cx| {
        window.click(access, cx);
        window.press("secondary-enter", cx);
    });
    window
}

fn console_status(window: &Window) -> String {
    label_of(window, "console-status")
        .map(|label| label.to_string())
        .unwrap_or_default()
}

#[gpui_kit::test]
fn runs_a_query_and_shows_its_rows(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = run_in_new_query(
        &fixture,
        "SELECT name FROM customers ORDER BY id DESC",
        "read-only",
        cx,
    );

    wait_until(window, cx, "the query finishes", |window, _| {
        console_status(window).starts_with("3 rows")
    });
    interact(window, cx, |window, _| {
        assert_eq!(window.find("cell-0-0").label(), Some("Linus"));
        assert_eq!(window.find("cell-2-0").label(), Some("Ada"));
    });
}

#[gpui_kit::test]
fn rejects_a_script_without_running_any_of_it(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = run_in_new_query(
        &fixture,
        "DELETE FROM customers; INSERT INTO customers VALUES (4, 'Eve')",
        "read-write",
        cx,
    );

    wait_until(window, cx, "the run fails", |window, _| {
        let status = console_status(window);
        !status.is_empty() && !status.starts_with("Choose") && !status.ends_with('…')
    });
    let status = interact(window, cx, |window, _| console_status(window));
    assert!(
        status.contains("one statement"),
        "unexpected status: {status}"
    );

    let customers: i64 = rusqlite::Connection::open(&fixture.database)
        .and_then(|connection| {
            connection.query_row("SELECT count(*) FROM customers", [], |row| row.get(0))
        })
        .expect("count customers");
    assert_eq!(customers, 3);
}
