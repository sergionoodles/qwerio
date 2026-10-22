//! Fitting grid columns to their content: a double-click on a column's
//! resize handle, or Fit All Columns from the row menu.

use gpui_kit::test::{ClickOptions, TestWindowExt as _};
use gpui_kit::{MouseButton, Pixels, TestAppContext, point, px};

use super::console_flows::{console_status, run_in_new_query};
use super::schema_flows::open_connection;
use super::*;

/// Inside the resize handle that straddles a header's right edge.
const RESIZE_HANDLE_INSET: Pixels = px(2.);
/// The widest a column starts, for any header.
const MAX_START_WIDTH: Pixels = px(320.);

fn column_width(window: &Window, col_ix: usize) -> Pixels {
    window.find(("col-header", col_ix)).bounds().size.width
}

fn double_click_resize_handle(window: &mut Window, col_ix: usize, cx: &mut App) {
    let bounds = window.find(("col-header", col_ix)).bounds();
    let edge = point(
        bounds.size.width - RESIZE_HANDLE_INSET,
        bounds.size.height / 2.,
    );
    let options = ClickOptions::new().with_offset(edge).with_count(2);

    window.click_with_options(("col-header", col_ix), options, cx);
}

#[gpui_kit::test]
fn double_clicking_a_resize_handle_narrows_a_column_to_short_values(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_connection(&fixture, true, cx);
    let before = interact(window, cx, |window, _| column_width(window, 1));

    interact(window, cx, |window, cx| {
        double_click_resize_handle(window, 1, cx)
    });

    // "name" and the customers' names are shorter than the starting width.
    let after = interact(window, cx, |window, _| column_width(window, 1));
    assert!(
        after < before,
        "{after:?} should be narrower than {before:?}"
    );
    assert!(after > px(0.));
}

#[gpui_kit::test]
fn double_clicking_a_resize_handle_widens_a_column_to_a_long_value(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let note = "a value much longer than any column starts out, which fits".repeat(2);
    let sql = format!("SELECT '{note}' AS note");
    let window = run_in_new_query(&fixture, &sql, "read-only", cx);
    wait_until(window, cx, "the query finishes", |window, _| {
        console_status(window).starts_with("1 row")
    });
    let before = interact(window, cx, |window, _| column_width(window, 0));
    assert!(before < MAX_START_WIDTH);

    interact(window, cx, |window, cx| {
        double_click_resize_handle(window, 0, cx)
    });

    let after = interact(window, cx, |window, _| column_width(window, 0));
    assert!(
        after > MAX_START_WIDTH,
        "{after:?} should hold the whole value"
    );
}

#[gpui_kit::test]
fn fit_all_columns_fits_every_column_as_its_resize_handle_does(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_connection(&fixture, true, cx);
    let widths = |window: &Window| [column_width(window, 0), column_width(window, 1)];
    let before = interact(window, cx, |window, _| widths(window));

    interact(window, cx, |window, cx| {
        click_row_header(window, 0, MouseButton::Right, cx)
    });
    wait_until(window, cx, "the row menu opens", |window, _| {
        window.try_find("popup-menu").is_some()
    });
    interact(window, cx, |window, cx| {
        click_menu_item(window, "Fit All Columns", cx)
    });
    let fitted = interact(window, cx, |window, _| widths(window));
    assert_ne!(fitted, before);

    // Each resize handle finds the columns already fitted.
    interact(window, cx, |window, cx| {
        double_click_resize_handle(window, 0, cx)
    });
    interact(window, cx, |window, cx| {
        double_click_resize_handle(window, 1, cx)
    });
    interact(window, cx, |window, _| assert_eq!(widths(window), fitted));
}
