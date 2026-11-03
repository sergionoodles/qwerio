//! Running statements from a temporary query.

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;

use super::*;

const TARGET_LABEL: &str = "Shop data (SQLite)";

/// Opens a temporary query, types `sql` into its editor (focused when the
/// query opens), chooses the shop connection and `access`, and runs it with
/// the keyboard shortcut.
pub(super) fn run_in_new_query(
    fixture: &Fixture,
    sql: &str,
    access: &'static str,
    cx: &mut TestAppContext,
) -> AnyWindowHandle {
    let window = prepare_new_query(fixture, sql, access, cx);
    interact(window, cx, |window, cx| window.press("secondary-enter", cx));
    window
}

fn prepare_new_query(
    fixture: &Fixture,
    sql: &str,
    access: &'static str,
    cx: &mut TestAppContext,
) -> AnyWindowHandle {
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the workspace opens", |window, _| {
        window.try_find("new-query").is_some()
    });

    interact(window, cx, |window, cx| window.click("new-query", cx));
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
        assert_eq!(window.find("console-target").focused(), Some(true));
    });
    window
}

pub(super) fn console_status(window: &Window) -> String {
    label_of(window, "console-status")
        .map(|label| label.to_string())
        .unwrap_or_default()
}

#[gpui_kit::test]
fn runs_a_query_from_the_focused_connection_picker_and_shows_its_rows(cx: &mut TestAppContext) {
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

#[gpui_kit::test]
fn run_all_uses_one_session_and_shows_the_last_result(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = prepare_new_query(
        &fixture,
        "BEGIN; CREATE TEMP TABLE script_values (value TEXT);
         INSERT INTO script_values VALUES ('a;b'); COMMIT; SELECT value FROM script_values;",
        "read-write",
        cx,
    );
    interact(window, cx, |window, cx| {
        window.press("secondary-shift-enter", cx)
    });
    wait_until(window, cx, "the script finishes", |window, _| {
        console_status(window).starts_with("5/5 statements completed · 1 rows")
    });
    interact(window, cx, |window, _| {
        assert_eq!(window.find("cell-0-0").label(), Some("a;b"));
        assert_ne!(window.find("run-all").disabled(), Some(true));
    });
}

#[gpui_kit::test]
fn run_all_stops_on_the_first_error_and_keeps_completed_changes(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = prepare_new_query(
        &fixture,
        "INSERT INTO customers VALUES (4, 'Eve'); SELECT * FROM missing_table; DELETE FROM customers;",
        "read-write",
        cx,
    );
    interact(window, cx, |window, cx| window.click("run-all", cx));
    wait_until(window, cx, "the script stops on the error", |window, _| {
        let status = console_status(window);
        status.starts_with("1/3 statements completed") && status.contains("missing_table")
    });
    let customers: i64 = rusqlite::Connection::open(&fixture.database)
        .and_then(|connection| {
            connection.query_row("SELECT count(*) FROM customers", [], |row| row.get(0))
        })
        .expect("count customers");
    assert_eq!(customers, 4);
}

#[gpui_kit::test]
fn run_all_respects_read_only_access(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = prepare_new_query(
        &fixture,
        "SELECT 1; DELETE FROM customers; SELECT 2;",
        "read-only",
        cx,
    );
    interact(window, cx, |window, cx| window.click("run-all", cx));
    wait_until(window, cx, "the write is rejected", |window, _| {
        let status = console_status(window);
        status.starts_with("1/3 statements completed") && !status.ends_with('…')
    });
    interact(window, cx, |window, _| {
        let status = console_status(window);
        assert!(
            status.contains("readonly") || status.contains("read-only"),
            "unexpected status: {status}"
        );
        assert_eq!(window.find("cell-0-0").label(), Some("1"));
    });
    let customers: i64 = rusqlite::Connection::open(&fixture.database)
        .and_then(|connection| {
            connection.query_row("SELECT count(*) FROM customers", [], |row| row.get(0))
        })
        .expect("count customers");
    assert_eq!(customers, 3);
}

#[gpui_kit::test]
fn cancelling_run_all_skips_later_statements(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = prepare_new_query(
        &fixture,
        "WITH RECURSIVE numbers(value) AS (SELECT 1 UNION ALL SELECT value + 1 FROM numbers WHERE value < 1000000000)
         SELECT sum(value) FROM numbers; DELETE FROM customers;",
        "read-write",
        cx,
    );
    interact(window, cx, |window, cx| window.click("run-all", cx));
    wait_until(window, cx, "the first statement starts", |window, _| {
        console_status(window).starts_with("Running statement 1 of 2")
    });
    interact(window, cx, |window, cx| {
        assert_eq!(window.find("run-all").disabled(), Some(true));
        window.click("cancel", cx);
    });
    wait_until(window, cx, "the script is cancelled", |window, _| {
        console_status(window).starts_with("0/2 statements completed · Cancelled")
    });
    let customers: i64 = rusqlite::Connection::open(&fixture.database)
        .and_then(|connection| {
            connection.query_row("SELECT count(*) FROM customers", [], |row| row.get(0))
        })
        .expect("count customers");
    assert_eq!(customers, 3);
}
