//! Selecting rows with Ctrl and Shift clicks and exporting them, or every
//! matching row, as CSV, JSON, or SQL INSERT to the clipboard or a file.

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{Modifiers, TestAppContext};

use super::schema_flows::open_connection;
use super::workspace_files::wait_for_picker;
use super::*;

/// Opens the export dialog, picks `format`, and clicks `destination`.
fn export_as(
    window: AnyWindowHandle,
    cx: &mut TestAppContext,
    scope: Option<&'static str>,
    format: &'static str,
    destination: &'static str,
) {
    interact(window, cx, |window, cx| window.click("export", cx));
    wait_until(window, cx, "the export dialog opens", |window, _| {
        window.try_find(destination).is_some()
    });
    interact(window, cx, |window, cx| {
        if let Some(scope) = scope {
            window.click(scope, cx);
        }
        window.click(format, cx);
    });
    interact(window, cx, |window, cx| window.click(destination, cx));
}

fn wait_for_clipboard(window: AnyWindowHandle, cx: &mut TestAppContext, expected: &str) {
    wait_until(window, cx, "the export reaches the clipboard", |_, cx| {
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .is_some_and(|text| text == expected)
    });
}

#[gpui_kit::test]
fn ctrl_and_shift_clicks_select_rows_to_copy_as_json_or_sql(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_connection(&fixture, true, cx);

    interact(window, cx, |window, cx| window.click("cell-0-1", cx));
    let secondary_key = cx.update(|cx| Modifiers::secondary_key(cx));
    holding(window, cx, secondary_key, |window, cx| {
        window.click("cell-2-1", cx)
    });
    interact(window, cx, |window, _| {
        assert_eq!(
            label_of(window, "status").as_deref(),
            Some("3 rows · 2 selected")
        );
    });

    export_as(window, cx, None, "export-format-json", "export-copy");
    wait_for_clipboard(
        window,
        cx,
        "[\n  {\"id\": 1, \"name\": \"Ada\"},\n  {\"id\": 3, \"name\": \"Linus\"}\n]\n",
    );

    // Shift selects the range from the last plain click.
    interact(window, cx, |window, cx| window.click("cell-0-1", cx));
    holding(window, cx, Modifiers::shift(), |window, cx| {
        window.click("cell-1-1", cx)
    });
    export_as(window, cx, None, "export-format-sql", "export-copy");
    wait_for_clipboard(
        window,
        cx,
        "INSERT INTO \"customers\" (\"id\", \"name\") VALUES (1, 'Ada');\n\
         INSERT INTO \"customers\" (\"id\", \"name\") VALUES (2, 'Grace');\n",
    );
}

#[gpui_kit::test]
fn regression_ctrl_click_on_a_row_header_unselects_the_row(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_connection(&fixture, true, cx);
    let ctrl_click_row_two = |cx: &mut TestAppContext| {
        let secondary_key = cx.update(|cx| Modifiers::secondary_key(cx));
        holding(window, cx, secondary_key, |window, cx| {
            click_row_header(window, 2, MouseButton::Left, cx)
        })
    };

    interact(window, cx, |window, cx| {
        click_row_header(window, 0, MouseButton::Left, cx)
    });
    ctrl_click_row_two(cx);
    interact(window, cx, |window, _| {
        assert_eq!(
            label_of(window, "status").as_deref(),
            Some("3 rows · 2 selected")
        );
    });

    ctrl_click_row_two(cx);
    interact(window, cx, |window, _| {
        assert_eq!(label_of(window, "status").as_deref(), Some("3 rows"));
        assert_eq!(window.find(("row", 2_usize)).selected(), Some(false));
    });
}

#[gpui_kit::test]
fn every_matching_row_is_saved_to_a_file(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_connection(&fixture, true, cx);
    let path = fixture.root.join("customers.csv");

    // One row is selected, but the whole table is exported.
    interact(window, cx, |window, cx| window.click("cell-1-1", cx));
    export_as(
        window,
        cx,
        Some("export-scope-all"),
        "export-format-csv",
        "export-save",
    );
    wait_for_picker(cx, true);
    cx.simulate_new_path_selection(|_| Some(path.clone()));

    let expected = "\"id\",\"name\"\n\"1\",\"Ada\"\n\"2\",\"Grace\"\n\"3\",\"Linus\"\n";
    wait_until(window, cx, "the file is written", |window, _| {
        window.try_find("export-progress").is_none()
            && std::fs::read_to_string(&path).is_ok_and(|text| text == expected)
    });
}

#[gpui_kit::test]
fn the_preview_follows_the_csv_separator_and_header_options(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_connection(&fixture, true, cx);

    interact(window, cx, |window, cx| window.click("export", cx));
    wait_until(window, cx, "the export dialog opens", |window, _| {
        window.try_find("export-preview").is_some()
    });
    interact(window, cx, |window, _| {
        assert_eq!(
            label_of(window, "export-preview").as_deref(),
            Some("\"id\",\"name\"\n\"1\",\"Ada\"\n\"2\",\"Grace\"\n\"3\",\"Linus\"\n")
        );
    });

    interact(window, cx, |window, cx| {
        window.click("export-separator-semicolon", cx);
        window.click("export-include-header", cx);
    });
    let expected = "\"1\";\"Ada\"\n\"2\";\"Grace\"\n\"3\";\"Linus\"\n";
    interact(window, cx, |window, _| {
        assert_eq!(
            label_of(window, "export-preview").as_deref(),
            Some(expected)
        );
    });

    interact(window, cx, |window, cx| window.click("export-copy", cx));
    wait_for_clipboard(window, cx, expected);
}
