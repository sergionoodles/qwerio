//! Adding, duplicating, and deleting rows, saved in one transaction with
//! cell edits, and discarded or undone before saving.

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;

use super::edit_flows::{open_tasks, stored_tasks};
use super::*;

#[gpui_kit::test]
fn clearing_default_text_inserts_an_empty_string(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_tasks(&fixture, cx);

    interact(window, cx, |window, cx| window.click("add-row", cx));
    interact(window, cx, |window, cx| window.press("tab", cx));
    interact(window, cx, |window, cx| window.press("enter", cx));
    interact(window, cx, |window, cx| window.input("temporary", cx));
    interact(window, cx, |window, cx| {
        window.press("ctrl-a", cx);
        window.press("backspace", cx);
    });
    interact(window, cx, |window, cx| window.press("enter", cx));
    interact(window, cx, |window, cx| window.click("save-edits", cx));
    wait_until(window, cx, "empty title inserted", |window, _| {
        window.try_find("save-edits").is_none()
    });
    let tasks = stored_tasks(&fixture);
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[2], (String::new(), None, None, None));
}

#[gpui_kit::test]
fn adds_duplicates_and_deletes_rows_in_one_save(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_tasks(&fixture, cx);

    // A new row goes on top, editing its first column; Tab moves on and
    // unset columns keep their default.
    interact(window, cx, |window, cx| window.click("add-row", cx));
    interact(window, cx, |window, cx| {
        assert_eq!(window.find("cell-editor").value(), Some(""));
        window.press("tab", cx);
    });
    interact(window, cx, |window, cx| window.press("enter", cx));
    interact(window, cx, |window, cx| {
        window.input("Plan", cx);
        window.press("enter", cx);
    });
    interact(window, cx, |window, _| {
        assert_eq!(label_of(window, "cell-0-0").as_deref(), Some("DEFAULT"));
        assert_eq!(label_of(window, "cell-0-1").as_deref(), Some("Plan"));
        assert_eq!(label_of(window, "cell-1-1").as_deref(), Some("Write"));
    });

    // Duplicating copies everything but the key.
    interact(window, cx, |window, cx| window.click("cell-2-1", cx));
    interact(window, cx, |window, cx| window.click("duplicate-row", cx));
    interact(window, cx, |window, cx| {
        assert_eq!(label_of(window, "cell-0-0").as_deref(), Some("DEFAULT"));
        assert_eq!(label_of(window, "cell-0-1").as_deref(), Some("Ship"));
        assert_eq!(window.find("cell-check-0-2").checked(), Some(true));
        window.double_click("cell-0-1", cx);
    });
    interact(window, cx, |window, cx| {
        window.input("Ship again", cx);
        window.press("enter", cx);
    });

    // Delete marks a loaded row; it is removed when saved.
    interact(window, cx, |window, cx| {
        window.click("cell-2-1", cx);
    });
    interact(window, cx, |window, cx| {
        window.press("delete", cx);
    });
    interact(window, cx, |window, cx| {
        assert_eq!(
            label_of(window, "delete-row").as_deref(),
            Some("Restore Row")
        );
        assert_eq!(
            label_of(window, "unsaved-changes").as_deref(),
            Some("3 unsaved changes")
        );
        assert_eq!(stored_tasks(&fixture).len(), 2, "nothing is saved yet");
        window.click("save-edits", cx);
    });
    wait_until(window, cx, "the changes are saved", |window, _| {
        window.try_find("save-edits").is_none()
    });

    assert_eq!(
        stored_tasks(&fixture),
        vec![
            ("Ship".to_owned(), Some(1), None, None),
            ("Plan".to_owned(), None, None, None),
            ("Ship again".to_owned(), Some(1), None, None),
        ]
    );
    interact(window, cx, |window, _| {
        assert_eq!(label_of(window, "cell-0-0").as_deref(), Some("4"));
        assert_eq!(label_of(window, "cell-0-1").as_deref(), Some("Ship again"));
        assert_eq!(label_of(window, "cell-1-1").as_deref(), Some("Plan"));
        assert_eq!(label_of(window, "cell-2-1").as_deref(), Some("Ship"));
        assert_eq!(label_of(window, "status").as_deref(), Some("3 rows"));
    });
}

#[gpui_kit::test]
fn removes_restores_and_discards_row_changes(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_tasks(&fixture, cx);

    // Shortcuts: a new row, then Delete removes it again.
    interact(window, cx, |window, cx| {
        window.click("cell-0-1", cx);
    });
    interact(window, cx, |window, cx| {
        window.press("alt-insert", cx);
    });
    interact(window, cx, |window, cx| {
        assert_eq!(label_of(window, "cell-1-1").as_deref(), Some("Write"));
        window.press("escape", cx);
        window.press("delete", cx);
    });
    interact(window, cx, |window, _| {
        assert_eq!(label_of(window, "cell-0-1").as_deref(), Some("Write"));
        assert!(window.try_find("save-edits").is_none());
    });

    // A row marked for deletion is not edited; Delete again restores it.
    interact(window, cx, |window, cx| {
        window.click("cell-0-1", cx);
    });
    interact(window, cx, |window, cx| {
        window.press("delete", cx);
    });
    interact(window, cx, |window, cx| window.double_click("cell-0-1", cx));
    interact(window, cx, |window, cx| {
        assert!(window.try_find("cell-editor").is_none());
        assert_eq!(
            label_of(window, "status").as_deref(),
            Some("This row is marked for deletion")
        );
        window.press("delete", cx);
    });
    interact(window, cx, |window, _| {
        assert!(window.try_find("save-edits").is_none());
    });

    // Discard drops a duplicate and a deletion together.
    interact(window, cx, |window, cx| {
        window.click("cell-1-1", cx);
    });
    interact(window, cx, |window, cx| {
        window.press("ctrl-d", cx);
    });
    interact(window, cx, |window, cx| {
        assert_eq!(label_of(window, "cell-0-1").as_deref(), Some("Ship"));
        window.click("cell-1-1", cx);
    });
    interact(window, cx, |window, cx| {
        window.press("delete", cx);
    });
    interact(window, cx, |window, cx| window.click("discard-edits", cx));
    interact(window, cx, |window, _| {
        assert_eq!(label_of(window, "cell-0-1").as_deref(), Some("Write"));
        assert_eq!(label_of(window, "cell-1-1").as_deref(), Some("Ship"));
        assert!(window.try_find("save-edits").is_none());
    });
    assert_eq!(stored_tasks(&fixture).len(), 2);
}
