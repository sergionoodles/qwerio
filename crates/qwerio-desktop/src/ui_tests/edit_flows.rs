//! Inline cell editing: typed editors, checkboxes, the calendar, NULL, and
//! saving staged edits together, and the prompt before leaving unsaved edits.

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;
use qwerio_core::display::NULL_LABEL;

use super::*;

/// Columns: id 0, title 1, done 2, due 3, note 4.
const TASKS: &str = "CREATE TABLE tasks (id INTEGER PRIMARY KEY, title TEXT NOT NULL, \
                     done BOOLEAN, due DATE, note TEXT);
                     INSERT INTO tasks VALUES (1, 'Write', 0, '2024-01-15', 'call'),
                                              (2, 'Ship', 1, NULL, NULL);";

/// Opens the `tasks` table, listed last after customers and orders.
pub(super) fn open_tasks(fixture: &Fixture, cx: &mut TestAppContext) -> AnyWindowHandle {
    rusqlite::Connection::open(&fixture.database)
        .and_then(|connection| connection.execute_batch(TASKS))
        .expect("create tasks");

    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);

    let connection = fixture.connection_key.clone();
    wait_until(window, cx, "the connection is listed", |window, _| {
        window.try_find(connection.clone()).is_some()
    });
    interact(window, cx, |window, cx| {
        window.double_click(connection.clone(), cx)
    });
    wait_until(window, cx, "the tables are listed", |window, _| {
        label_of(window, "status").as_deref() == Some("3 items")
    });
    interact(window, cx, |window, cx| {
        window.press("home", cx);
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    wait_until(window, cx, "the tasks load", |window, _| {
        label_of(window, "status").as_deref() == Some("2 rows")
    });

    window
}

/// A stored task: title, done, due, and note.
pub(super) type StoredTask = (String, Option<i64>, Option<String>, Option<String>);

pub(super) fn stored_tasks(fixture: &Fixture) -> Vec<StoredTask> {
    let connection = rusqlite::Connection::open(&fixture.database).expect("open database");
    let mut statement = connection
        .prepare("SELECT title, done, due, note FROM tasks ORDER BY id")
        .expect("prepare");
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("query tasks")
        .collect::<Result<_, _>>()
        .expect("tasks")
}

#[gpui_kit::test]
fn clearing_null_text_stores_an_empty_string(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_tasks(&fixture, cx);

    interact(window, cx, |window, cx| window.double_click("cell-1-4", cx));
    interact(window, cx, |window, cx| window.press("enter", cx));
    interact(window, cx, |window, _| {
        assert!(window.try_find("save-edits").is_none());
    });

    interact(window, cx, |window, cx| window.double_click("cell-1-4", cx));
    interact(window, cx, |window, cx| window.input("temporary", cx));
    interact(window, cx, |window, cx| {
        window.press("ctrl-a", cx);
        window.press("backspace", cx);
    });
    interact(window, cx, |window, cx| window.press("enter", cx));
    interact(window, cx, |window, _| {
        assert!(window.try_find("save-edits").is_some());
    });
    assert!(stored_tasks(&fixture)[1].3.is_none());

    interact(window, cx, |window, cx| window.click("save-edits", cx));
    wait_until(window, cx, "empty note saved", |window, _| {
        window.try_find("save-edits").is_none()
    });
    assert_eq!(stored_tasks(&fixture)[1].3.as_deref(), Some(""));
}

#[gpui_kit::test]
fn edits_cells_with_typed_editors_and_saves_them_together(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_tasks(&fixture, cx);

    // Text: the editor starts with the value selected, so typing replaces it.
    interact(window, cx, |window, cx| window.double_click("cell-0-1", cx));
    interact(window, cx, |window, cx| {
        assert_eq!(window.find("cell-editor").value(), Some("Write"));
        assert!(
            window.try_find("cell-editor-null").is_none(),
            "title does not allow NULL"
        );
        window.input("Draft", cx);
        window.press("enter", cx);
    });
    interact(window, cx, |window, _| {
        assert!(window.try_find("cell-editor").is_none());
        assert_eq!(label_of(window, "cell-0-1").as_deref(), Some("Draft"));
    });

    // Booleans are checkboxes.
    interact(window, cx, |window, cx| window.click("cell-check-0-2", cx));
    interact(window, cx, |window, _| {
        assert_eq!(window.find("cell-check-0-2").checked(), Some(true));
    });

    // Dates: the calendar opens at the cell's month and picking a day is done.
    interact(window, cx, |window, cx| window.double_click("cell-0-3", cx));
    interact(window, cx, |window, cx| {
        window.click("cell-editor-calendar", cx)
    });
    interact(window, cx, |window, cx| {
        window.click("calendar-2024-01-20-0-2", cx)
    });
    interact(window, cx, |window, _| {
        assert!(window.try_find("cell-editor").is_none());
        assert_eq!(label_of(window, "cell-0-3").as_deref(), Some("2024-01-20"));
    });

    // NULL from the editor of a nullable column.
    interact(window, cx, |window, cx| window.double_click("cell-0-4", cx));
    interact(window, cx, |window, cx| {
        window.click("cell-editor-null", cx)
    });
    interact(window, cx, |window, _| {
        assert_eq!(label_of(window, "cell-0-4").as_deref(), Some("NULL"));
    });

    // Invalid text keeps the editor open with the reason in the status bar.
    interact(window, cx, |window, cx| window.double_click("cell-1-3", cx));
    interact(window, cx, |window, cx| {
        window.input("2024-02-30", cx);
        window.press("enter", cx);
    });
    interact(window, cx, |window, cx| {
        assert!(window.try_find("cell-editor").is_some());
        assert_eq!(
            label_of(window, "status").as_deref(),
            Some("Enter a date as YYYY-MM-DD")
        );
        window.press("ctrl-a", cx);
        window.input("2024-03-01", cx);
        window.press("enter", cx);
    });

    interact(window, cx, |window, cx| {
        assert_eq!(label_of(window, "cell-1-3").as_deref(), Some("2024-03-01"));
        assert_eq!(stored_tasks(&fixture)[0].0, "Write", "nothing is saved yet");
        window.click("save-edits", cx);
    });
    wait_until(window, cx, "the edits are saved", |window, _| {
        window.try_find("save-edits").is_none()
    });

    assert_eq!(
        stored_tasks(&fixture),
        vec![
            (
                "Draft".to_owned(),
                Some(1),
                Some("2024-01-20".to_owned()),
                None
            ),
            (
                "Ship".to_owned(),
                Some(1),
                Some("2024-03-01".to_owned()),
                None
            ),
        ]
    );
}

#[gpui_kit::test]
fn regression_sorting_stages_open_cell_input(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_tasks(&fixture, cx);
    interact(window, cx, |window, cx| window.double_click("cell-0-1", cx));
    interact(window, cx, |window, cx| {
        window.input("Draft before sort", cx)
    });
    interact(window, cx, |window, cx| {
        window.click(("icon-sort", 1_usize), cx)
    });
    wait_until(
        window,
        cx,
        "typed input staged before sorting",
        |window, _| window.try_find("save-edits").is_some(),
    );
    wait_until(window, cx, "sorted rows loaded", |window, _| {
        label_of(window, "status").as_deref() == Some("2 rows")
    });
    interact(window, cx, |window, cx| window.click("save-edits", cx));
    wait_until(window, cx, "sorted edit saved", |window, _| {
        window.try_find("save-edits").is_none()
    });
    assert_eq!(stored_tasks(&fixture)[0].0, "Draft before sort");
}

#[gpui_kit::test]
fn regression_refresh_preserves_invalid_cell_input(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_tasks(&fixture, cx);
    interact(window, cx, |window, cx| window.double_click("cell-0-3", cx));
    interact(window, cx, |window, cx| window.input("invalid date", cx));
    interact(window, cx, |window, cx| {
        window.dispatch_action(Box::new(crate::actions::Refresh), cx)
    });
    // Wait through an attempted metadata load; the invalid input must still be editable.
    wait_until(window, cx, "invalid input kept", |window, _| {
        label_of(window, "status").as_deref() == Some("Enter a date as YYYY-MM-DD")
    });
    interact(window, cx, |window, _| {
        assert_eq!(window.find("cell-editor").value(), Some("invalid date"))
    });
}

#[gpui_kit::test]
fn regression_saving_freezes_row_edits_and_navigation(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_tasks(&fixture, cx);
    interact(window, cx, |window, cx| window.click("cell-0-1", cx));
    interact(window, cx, |window, cx| window.click("duplicate-row", cx));
    let blocker = rusqlite::Connection::open(&fixture.database).expect("test database");
    blocker
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold a test write lock");
    interact(window, cx, |window, cx| window.click("save-edits", cx));
    wait_until(window, cx, "saving starts", |window, _| {
        label_of(window, "save-edits").as_deref() == Some("Saving…")
    });
    interact(window, cx, |window, cx| window.double_click("cell-0-1", cx));
    interact(window, cx, |window, _| {
        assert!(window.try_find("cell-editor").is_none())
    });
    interact(window, cx, |window, cx| window.click("back", cx));
    interact(window, cx, |window, _| {
        assert!(window.try_find("discard-leave").is_none());
        assert!(window.try_find("cell-0-1").is_some());
    });
    blocker
        .execute_batch("ROLLBACK")
        .expect("release test write lock");
    wait_until(window, cx, "save finishes", |window, _| {
        window.try_find("save-edits").is_none()
    });
    assert_eq!(stored_tasks(&fixture).len(), 3);
}

#[gpui_kit::test]
fn edits_from_the_keyboard_and_discards_changes(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_tasks(&fixture, cx);

    // Enter edits the selected cell; Escape cancels without staging.
    interact(window, cx, |window, cx| {
        window.click("cell-1-1", cx);
        window.press("enter", cx);
    });
    interact(window, cx, |window, cx| {
        assert_eq!(window.find("cell-editor").value(), Some("Ship"));
        window.input("Sail", cx);
        window.press("escape", cx);
    });
    interact(window, cx, |window, _| {
        assert!(window.try_find("cell-editor").is_none());
        assert_eq!(label_of(window, "cell-1-1").as_deref(), Some("Ship"));
        assert!(window.try_find("save-edits").is_none());
    });

    // Moving to another cell commits the edit.
    interact(window, cx, |window, cx| window.press("f2", cx));
    interact(window, cx, |window, cx| {
        window.input("Sail", cx);
        window.press("up", cx);
    });
    interact(window, cx, |window, cx| {
        assert!(window.try_find("cell-editor").is_none());
        assert_eq!(label_of(window, "cell-1-1").as_deref(), Some("Sail"));
        window.click("discard-edits", cx);
    });
    interact(window, cx, |window, _| {
        assert_eq!(label_of(window, "cell-1-1").as_deref(), Some("Ship"));
        assert!(window.try_find("save-edits").is_none());
    });
    assert_eq!(stored_tasks(&fixture)[1].0, "Ship");
}

#[gpui_kit::test]
fn leaving_a_table_with_unsaved_edits_asks_first(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_tasks(&fixture, cx);

    interact(window, cx, |window, cx| window.double_click("cell-0-1", cx));
    interact(window, cx, |window, cx| {
        window.input("Draft", cx);
        window.press("enter", cx);
    });

    // Going back asks; cancelling stays on the table with the edit staged.
    interact(window, cx, |window, cx| window.press("alt-left", cx));
    interact(window, cx, |window, cx| window.click("cancel-leave", cx));
    interact(window, cx, |window, _| {
        assert!(window.try_find("discard-leave").is_none());
        assert_eq!(label_of(window, "cell-0-1").as_deref(), Some("Draft"));
    });

    // Closing the tab and saving writes the edit, then closes the tab.
    interact(window, cx, |window, cx| window.press("ctrl-w", cx));
    interact(window, cx, |window, cx| window.click("save-leave", cx));
    wait_until(window, cx, "the table closes", |window, _| {
        window.try_find("cell-0-1").is_none()
    });
    assert_eq!(stored_tasks(&fixture)[0].0, "Draft");
}

#[gpui_kit::test]
fn closing_workspace_guards_pending_rows(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_tasks(&fixture, cx);
    interact(window, cx, |window, cx| window.double_click("cell-0-1", cx));
    interact(window, cx, |window, cx| {
        window.input("Pending", cx);
        window.press("enter", cx);
        window.dispatch_action(Box::new(crate::actions::CloseWorkspace), cx);
    });
    wait_until(window, cx, "workspace leave prompt", |window, _| {
        window.try_find("cancel-leave").is_some()
    });
    assert_eq!(stored_tasks(&fixture)[0].0, "Write");
    interact(window, cx, |window, cx| window.click("cancel-leave", cx));
    interact(window, cx, |window, _| {
        assert_eq!(label_of(window, "cell-0-1").as_deref(), Some("Pending"));
    });
    interact(window, cx, |window, cx| {
        window.click("cell-0-1", cx);
        window.dispatch_action(Box::new(crate::actions::CloseWorkspace), cx);
    });
    wait_until(window, cx, "save before close", |window, _| {
        window.try_find("save-leave").is_some()
    });
    interact(window, cx, |window, cx| window.click("save-leave", cx));
    wait_until(window, cx, "closed after saving", |window, _| {
        window.try_find("open-workspace").is_some()
    });
    assert_eq!(stored_tasks(&fixture)[0].0, "Pending");
}

#[gpui_kit::test]
fn workspace_file_commands_guard_pending_rows_before_opening_pickers(cx: &mut TestAppContext) {
    use crate::actions::{ImportWorkspace, NewWorkspace, OpenWorkspace};
    let commands: [(Box<dyn gpui_kit::Action>, bool); 3] = [
        (Box::new(OpenWorkspace), false),
        (Box::new(NewWorkspace), true),
        (Box::new(ImportWorkspace), false),
    ];
    for (command, destination) in commands {
        let fixture = Fixture::new();
        let window = open_tasks(&fixture, cx);
        interact(window, cx, |window, cx| window.double_click("cell-0-1", cx));
        interact(window, cx, |window, cx| {
            window.input("Pending", cx);
            window.press("enter", cx);
            window.dispatch_action(command.boxed_clone(), cx);
        });
        wait_until(window, cx, "workspace leave prompt", |window, _| {
            window.try_find("cancel-leave").is_some()
        });
        assert!(!cx.did_prompt_for_paths() && !cx.did_prompt_for_new_path());
        interact(window, cx, |window, cx| window.click("cancel-leave", cx));
        interact(window, cx, |window, cx| {
            window.dispatch_action(command.boxed_clone(), cx)
        });
        wait_until(window, cx, "leave prompt again", |window, _| {
            window.try_find("discard-leave").is_some()
        });
        interact(window, cx, |window, cx| window.click("discard-leave", cx));
        super::workspace_files::wait_for_picker(cx, destination);
        assert_eq!(stored_tasks(&fixture)[0].0, "Write");
        if destination {
            assert!(cx.did_prompt_for_new_path());
            cx.simulate_new_path_selection(|_| None);
        } else {
            assert!(cx.did_prompt_for_paths());
            cx.simulate_path_prompt_response(|_| None);
        }
    }
}

#[gpui_kit::test]
fn regression_right_clicking_a_cell_opens_the_row_menu_for_that_cell(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_tasks(&fixture, cx);

    interact(window, cx, |window, cx| window.right_click("cell-0-4", cx));
    wait_until(window, cx, "the row menu opens", |window, _| {
        window.try_find("popup-menu").is_some()
    });
    interact(window, cx, |window, cx| {
        assert_eq!(
            window.within("popup-menu").find(0_usize).label(),
            Some("Inspect Row")
        );
        click_menu_item(window, "Set Cell to NULL", cx);
    });

    wait_until(window, cx, "the note is set to NULL", |window, _| {
        label_of(window, "cell-0-4").as_deref() == Some(NULL_LABEL)
    });
}
