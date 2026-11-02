//! Whole-database operations use the clicked database and require restore consent.

use gpui_kit::test::TestWindowExt as _;

use super::schema_flows::open_connection;
use super::workspace_files::wait_for_picker;
use super::*;

fn open_transfer(fixture: &Fixture, is_restore: bool, cx: &mut TestAppContext) -> AnyWindowHandle {
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "the connection is listed", |window, _| {
        window.try_find(fixture.connection_key.clone()).is_some()
    });
    interact(window, cx, |window, cx| {
        window.right_click(fixture.connection_key.clone(), cx)
    });
    wait_until(window, cx, "the database menu opens", |window, _| {
        window.try_find("popup-menu").is_some()
    });
    interact(window, cx, |window, cx| {
        let mut menu = window.within("popup-menu");
        assert_eq!(menu.find(6_usize).label(), Some("Backup Database…"));
        assert_eq!(menu.find(7_usize).label(), Some("Restore Database…"));
        menu.click(if is_restore { 7_usize } else { 6_usize }, cx);
    });
    wait_until(
        window,
        cx,
        "the dialog resolves the connection",
        |window, _| {
            label_of(window, "transfer-target")
                .is_some_and(|label| label == fixture.database.display().to_string())
        },
    );
    window
}

fn customer_count(path: &Path) -> i64 {
    rusqlite::Connection::open(path)
        .expect("database")
        .query_row("SELECT count(*) FROM customers", [], |row| row.get(0))
        .expect("customers")
}

#[gpui_kit::test]
fn database_context_menu_backs_up_the_entire_sqlite_database(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_transfer(&fixture, false, cx);
    let destination = fixture.root.join("backup.sqlite");
    interact(window, cx, |window, cx| {
        assert!(window.try_find("transfer-tool-directory").is_none());
        window.click("transfer-choose-file", cx);
    });
    wait_for_picker(cx, true);
    cx.simulate_new_path_selection(|_| Some(destination.clone()));
    wait_until(window, cx, "the destination is selected", |window, _| {
        window.find("transfer-file").value() == destination.to_str()
    });
    interact(window, cx, |window, cx| window.click("transfer-start", cx));
    wait_until(window, cx, "the backup completes", |window, _| {
        window.try_find("transfer-notice").is_some()
    });
    assert_eq!(customer_count(&destination), 3);
    assert_eq!(customer_count(&fixture.database), 3);
}

#[gpui_kit::test]
fn sqlite_restore_requires_confirmation_and_creates_a_separate_database(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_transfer(&fixture, true, cx);
    let destination = fixture.root.join("restored.sqlite");
    interact(window, cx, |window, cx| {
        window.click("transfer-choose-file", cx)
    });
    wait_for_picker(cx, false);
    cx.simulate_path_prompt_response(|_| Some(vec![fixture.database.clone()]));
    interact(window, cx, |window, cx| {
        window.click("transfer-choose-destination", cx)
    });
    wait_for_picker(cx, true);
    cx.simulate_new_path_selection(|_| Some(destination.clone()));
    wait_until(
        window,
        cx,
        "the restore destination is selected",
        |window, _| window.find("transfer-restored-file").value() == destination.to_str(),
    );
    interact(window, cx, |window, cx| window.click("transfer-start", cx));
    assert!(
        !destination.exists(),
        "restore must not start without confirmation"
    );
    interact(window, cx, |window, cx| {
        window.click("transfer-confirm-restore", cx)
    });
    interact(window, cx, |window, cx| window.click("transfer-start", cx));
    wait_until(window, cx, "the restore completes", |window, _| {
        window.try_find("transfer-notice").is_some()
    });
    assert_eq!(customer_count(&destination), 3);
    assert_eq!(customer_count(&fixture.database), 3);
}

#[gpui_kit::test]
fn failed_backup_preserves_an_existing_destination(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_transfer(&fixture, false, cx);
    let destination = fixture.root.join("existing.sqlite");
    std::fs::write(&destination, "keep this file").expect("existing file");
    interact(window, cx, |window, cx| {
        window.click("transfer-file", cx);
        window.input(&destination.display().to_string(), cx);
    });
    wait_until(window, cx, "the destination is entered", |window, _| {
        window.find("transfer-file").value() == destination.to_str()
    });
    interact(window, cx, |window, cx| window.click("transfer-start", cx));
    wait_until(window, cx, "the error is shown", |window, _| {
        label_of(window, "transfer-error").is_some_and(|message| message.contains("already exists"))
    });
    assert_eq!(
        std::fs::read_to_string(destination).expect("preserved file"),
        "keep this file"
    );
}

#[gpui_kit::test]
fn database_toolbar_offers_the_same_operations(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_connection(&fixture, false, cx);
    interact(window, cx, |window, cx| {
        window.click("database-actions", cx)
    });
    wait_until(
        window,
        cx,
        "the database actions menu opens",
        |window, _| window.try_find("popup-menu").is_some(),
    );
    interact(window, cx, |window, cx| {
        let mut menu = window.within("popup-menu");
        assert_eq!(menu.find(0_usize).label(), Some("Backup Database…"));
        assert_eq!(menu.find(1_usize).label(), Some("Restore Database…"));
        menu.click(1_usize, cx);
    });
    wait_until(
        window,
        cx,
        "restore opens for the current database",
        |window, _| window.try_find("transfer-restored-file").is_some(),
    );
}
