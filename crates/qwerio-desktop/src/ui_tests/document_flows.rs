//! Editing and saving workspace documents.

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;

use super::*;

#[gpui_kit::test]
fn edits_and_saves_a_note(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);

    let note = fixture.note_key.clone();
    wait_until(window, cx, "the note is listed", |window, _| {
        window.try_find(note.clone()).is_some()
    });
    interact(window, cx, |window, cx| {
        window.double_click(note.clone(), cx)
    });
    wait_until(window, cx, "the note opens", |window, _| {
        label_of(window, "status").is_some_and(|status| status.starts_with("Saved"))
    });

    interact(window, cx, |window, cx| {
        window.press("secondary-end", cx);
        window.input("Shipping rules.\n", cx);
    });
    wait_until(window, cx, "the note is modified", |window, _| {
        label_of(window, "status").is_some_and(|status| status.starts_with("Modified"))
    });

    interact(window, cx, |window, cx| window.press("secondary-s", cx));
    wait_until(window, cx, "the note is saved", |window, _| {
        label_of(window, "status").is_some_and(|status| status.starts_with("Saved"))
    });

    let saved = std::fs::read_to_string(fixture.root.join(NOTE_FILE)).expect("read note");
    assert_eq!(saved, format!("{NOTE_TEXT}Shipping rules.\n"));
}
