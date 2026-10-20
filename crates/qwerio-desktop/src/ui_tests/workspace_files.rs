//! Native file picker, encryption, import preservation, and label flows.
use super::*;
use crate::actions::{ExportWorkspace, ImportWorkspace, NewWorkspace, OpenWorkspace};
use gpui_kit::test::TestWindowExt as _;
use qwerio_core::connection::Secret;

pub(super) fn wait_for_picker(cx: &TestAppContext, destination: bool) {
    let deadline = Instant::now() + UI_TIMEOUT;
    loop {
        cx.run_until_parked();
        if if destination {
            cx.did_prompt_for_new_path()
        } else {
            cx.did_prompt_for_paths()
        } {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native file picker did not appear"
        );
        std::thread::sleep(POLL_INTERVAL);
        cx.executor().advance_clock(POLL_INTERVAL);
    }
}

fn choose_password(
    window: AnyWindowHandle,
    cx: &mut TestAppContext,
    password: &str,
    confirm: bool,
) {
    wait_until(window, cx, "password dialog", |window, _| {
        window.try_find("workspace-password").is_some()
    });
    interact(window, cx, |window, cx| {
        window.click("workspace-password", cx);
        window.input(password, cx);
        if confirm {
            window.click("workspace-password-confirm", cx);
            window.input(password, cx);
        }
        window.click("workspace-password-accept", cx);
    });
}

#[gpui_kit::test]
fn regression_new_database_is_stored_inside_the_encrypted_workspace(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let path = fixture.root.with_file_name("private.qwerio");
    let password = Secret::new("workspace password");
    let mut workspace =
        Workspace::create_file(&path, "Private", Some(&password)).expect("workspace");
    let existing = workspace
        .add_connection("Existing", EngineKind::Sqlite)
        .expect("existing connection");
    workspace
        .save_connection(qwerio_workspace::file::StoredConnection {
            id: existing,
            options: ConnectionOptions::Sqlite(SqliteOptions::new(&fixture.database)),
            password: None,
        })
        .expect("existing binding");
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(path.clone()), cx);
    choose_password(window, cx, password.expose(), false);
    wait_until(window, cx, "workspace", |window, _| {
        window.try_find("new-database").is_some()
    });
    interact(window, cx, |window, cx| {
        window.double_click(SharedString::from(format!("connection:{existing}")), cx)
    });
    wait_until(window, cx, "existing connection opens", |window, _| {
        label_of(window, "status").as_deref() == Some("2 items")
    });
    interact(window, cx, |window, cx| window.click("new-database", cx));
    wait_for_picker(cx, true);
    let database = fixture.root.with_file_name("new.sqlite");
    cx.simulate_new_path_selection(|_| Some(database.clone()));
    wait_until(window, cx, "connection added", |_, _| {
        Workspace::open_file(&path, Some(&password))
            .expect("workspace")
            .manifest()
            .connections
            .len()
            == 2
    });
    wait_until(window, cx, "new database opens", |window, _| {
        label_of(window, "status").as_deref() == Some("0 items")
    });
    let saved = Workspace::open_file(&path, Some(&password)).expect("workspace");
    let id = saved
        .manifest()
        .connections
        .iter()
        .find(|connection| connection.id != existing)
        .expect("new connection")
        .id;
    assert!(saved.stored_connection(id).is_some());
    assert!(
        fixture
            .services
            .local_state
            .binding(saved.manifest().id, id)
            .expect("local binding")
            .is_none()
    );
}

#[gpui_kit::test]
fn regression_import_does_not_write_global_labels_to_its_source(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    fixture
        .services
        .local_state
        .set_setting(
            crate::settings_dialog::labels::LABELS_SETTING,
            &vec!["custom global label"],
        )
        .expect("global labels");
    let source = fixture.root.with_file_name("import-source.qwerio");
    Workspace::create_file(&source, "Source", None).expect("source");
    let original = std::fs::read(&source).expect("source bytes");
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "workspace", |window, _| {
        window.try_find("new-note").is_some()
    });
    interact(window, cx, |window, cx| {
        window.dispatch_action(Box::new(ImportWorkspace), cx)
    });
    wait_for_picker(cx, false);
    cx.simulate_path_prompt_response(|_| Some(vec![source.clone()]));
    wait_for_picker(cx, true);
    cx.simulate_new_path_selection(|_| None);
    assert_eq!(std::fs::read(source).expect("source bytes"), original);
}

#[gpui_kit::test]
fn regression_canceling_startup_unlock_returns_to_welcome(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let path = fixture.root.with_file_name("last.qwerio");
    Workspace::create_file(&path, "Last", Some(&Secret::new("password"))).expect("workspace");
    fixture
        .services
        .local_state
        .record_recent_workspace(&path, "Last", SystemTime::now())
        .expect("recent");
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(None, cx);
    wait_until(window, cx, "unlock dialog", |window, _| {
        window.try_find("workspace-password-cancel").is_some()
    });
    interact(window, cx, |window, cx| {
        window.click("workspace-password-cancel", cx)
    });
    wait_until(window, cx, "welcome after cancellation", |window, _| {
        window.try_find("open-workspace").is_some()
    });
}

#[gpui_kit::test]
fn regression_reverting_a_document_clears_its_embedded_draft(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let path = fixture.root.with_file_name("drafts.qwerio");
    let password = Secret::new("password");
    let mut workspace =
        Workspace::create_file(&path, "Drafts", Some(&password)).expect("workspace");
    let (document, _) = workspace
        .create_document(Path::new("note.md"), Placement::Root, "Saved text")
        .expect("note");
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(path.clone()), cx);
    choose_password(window, cx, password.expose(), false);
    let key = SharedString::from(format!("document:{document}"));
    wait_until(window, cx, "note listed", |window, _| {
        window.try_find(key.clone()).is_some()
    });
    interact(window, cx, |window, cx| {
        window.double_click(key.clone(), cx)
    });
    wait_until(window, cx, "note ready", |window, _| {
        label_of(window, "status").is_some_and(|status| status.starts_with("Saved"))
    });
    interact(window, cx, |window, cx| {
        window.press("ctrl-a", cx);
        window.input("Abandoned draft", cx);
    });
    wait_until(window, cx, "note modified", |window, _| {
        label_of(window, "status").is_some_and(|status| status.starts_with("Modified"))
    });
    cx.executor().advance_clock(Duration::from_secs(2));
    wait_until(window, cx, "draft autosaved", |_, _| {
        Workspace::open_file(&path, Some(&password))
            .expect("workspace")
            .draft(document)
            == Some("Abandoned draft")
    });
    interact(window, cx, |window, cx| {
        window.press("ctrl-a", cx);
        window.input("Saved text", cx);
    });
    wait_until(window, cx, "note clean", |window, _| {
        label_of(window, "status").is_some_and(|status| status.starts_with("Saved"))
    });
    interact(window, cx, |window, cx| {
        window.dispatch_action(Box::new(crate::actions::CloseWorkspace), cx)
    });
    wait_until(window, cx, "workspace closed", |window, _| {
        window.try_find("open-workspace").is_some()
    });
    assert!(
        Workspace::open_file(&path, Some(&password))
            .expect("workspace")
            .draft(document)
            .is_none()
    );
}

#[gpui_kit::test]
fn exports_legacy_credentials_and_imports_an_independently_encrypted_file(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let legacy = Workspace::open(&fixture.root).expect("legacy");
    let alias = legacy.manifest().connections[0].clone();
    fixture
        .services
        .bind_connection(
            legacy.manifest(),
            alias.id,
            ConnectionOptions::Sqlite(SqliteOptions::new(&fixture.database)),
            Some(Secret::new("keyring secret")),
        )
        .expect("keyring credential");
    let note = legacy.manifest().documents[0].id;
    fixture
        .services
        .local_state
        .save_draft(
            legacy.manifest().id,
            &qwerio_app::local_state::Draft {
                document: note,
                text: "unsaved note".into(),
                base: None,
            },
            SystemTime::now(),
        )
        .expect("legacy draft");
    let export_path = fixture.root.with_file_name("export.qwerio");
    let import_path = fixture.root.with_file_name("import.qwerio");
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "legacy opens", |window, _| {
        window.try_find(fixture.connection_key.clone()).is_some()
    });
    interact(window, cx, |window, cx| {
        window.dispatch_action(Box::new(ExportWorkspace), cx)
    });
    wait_for_picker(cx, true);
    cx.simulate_new_path_selection(|_| Some(export_path.clone()));
    choose_password(window, cx, "export password", true);
    wait_until(window, cx, "export saved", |_, _| export_path.is_file());
    let exported = Workspace::open_file(&export_path, Some(&Secret::new("export password")))
        .expect("encrypted export");
    assert_eq!(
        exported
            .stored_connection(alias.id)
            .expect("connection")
            .password
            .as_ref()
            .map(Secret::expose),
        Some("keyring secret")
    );
    assert_eq!(exported.load_document(note).expect("note").text, NOTE_TEXT);
    assert_eq!(exported.draft(note), Some("unsaved note"));
    assert!(
        fixture
            .services
            .resolve_connection(legacy.manifest(), alias.id)
            .expect("original credential survives")
            .credentials
            .password
            .is_some()
    );

    interact(window, cx, |window, cx| {
        window.dispatch_action(Box::new(ImportWorkspace), cx)
    });
    wait_for_picker(cx, false);
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files && options.directories);
        Some(vec![export_path.clone()])
    });
    choose_password(window, cx, "export password", false);
    wait_for_picker(cx, true);
    cx.simulate_new_path_selection(|_| Some(import_path.clone()));
    choose_password(window, cx, "import password", true);
    wait_until(window, cx, "import activated", |window, _| {
        window.try_find("workspace-password").is_none()
            && window.try_find(fixture.connection_key.clone()).is_some()
            && import_path.is_file()
    });
    let imported = Workspace::open_file(&import_path, Some(&Secret::new("import password")))
        .expect("imported");
    assert_eq!(imported.manifest().id, legacy.manifest().id);
    assert_eq!(
        imported.stored_connection(alias.id),
        exported.stored_connection(alias.id)
    );
    assert!(Workspace::open_file(&import_path, Some(&Secret::new("export password"))).is_err());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join(NOTE_FILE)).expect("legacy file untouched"),
        NOTE_TEXT
    );
}

#[gpui_kit::test]
fn creates_a_file_and_opens_it_with_the_native_picker(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let path = fixture.root.with_file_name("New.qwerio");
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(fixture.root.clone()), cx);
    wait_until(window, cx, "workspace", |window, _| {
        window.try_find("new-note").is_some()
    });
    interact(window, cx, |window, cx| {
        window.dispatch_action(Box::new(NewWorkspace), cx)
    });
    wait_for_picker(cx, true);
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    wait_until(window, cx, "encryption choice", |window, _| {
        window.try_find("workspace-without-password").is_some()
    });
    interact(window, cx, |window, cx| {
        window.click("workspace-without-password", cx)
    });
    wait_until(window, cx, "new workspace", |window, _| {
        label_of(window, "place-root").as_deref() == Some("New")
    });
    interact(window, cx, |window, cx| window.click("new-note", cx));
    wait_until(window, cx, "note editor", |window, _| {
        label_of(window, "status").is_some_and(|status| status.starts_with("Saved"))
    });
    interact(window, cx, |window, cx| {
        window.input("A portable note", cx);
        window.press("ctrl-s", cx);
    });
    wait_until(window, cx, "note saved", |window, _| {
        label_of(window, "status").is_some_and(|status| status.starts_with("Saved"))
    });
    let saved = Workspace::open_file(&path, None).expect("file");
    let note = saved.manifest().documents[0].id;
    assert_eq!(
        saved.load_document(note).expect("embedded note").text,
        "A portable note"
    );
    interact(window, cx, |window, cx| {
        window.dispatch_action(Box::new(crate::actions::CloseWorkspace), cx)
    });
    wait_until(window, cx, "workspace closed", |window, _| {
        window.try_find("open-workspace").is_some()
    });
    interact(window, cx, |window, cx| {
        window.dispatch_action(Box::new(OpenWorkspace), cx)
    });
    wait_for_picker(cx, false);
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files && !options.directories);
        Some(vec![path.clone()])
    });
    wait_until(window, cx, "file reopened", |window, _| {
        label_of(window, "place-root").as_deref() == Some("New")
            && label_of(window, "status").is_some_and(|status| status.starts_with("Saved"))
    });
}

#[gpui_kit::test]
fn defines_and_assigns_labels_and_preserves_pending_database_rows(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = super::edit_flows::open_tasks(&fixture, cx);
    interact(window, cx, |window, cx| window.double_click("cell-0-1", cx));
    interact(window, cx, |window, cx| {
        window.input("Pending", cx);
        window.press("enter", cx);
        window.click("settings", cx);
    });
    wait_until(window, cx, "settings", |window, _| {
        window.try_find("settings-section-labels").is_some()
    });
    interact(window, cx, |window, cx| {
        window.click("settings-section-labels", cx);
        window.click("new-label-name", cx);
        window.input("analytics", cx);
        window.click("add-label", cx);
    });
    wait_until(window, cx, "label persisted", |_, _| {
        Workspace::open(&fixture.root)
            .expect("workspace")
            .manifest()
            .labels
            .contains(&"analytics".into())
    });
    assert_eq!(
        super::edit_flows::stored_tasks(&fixture)[0].0,
        "Write",
        "workspace autosave never saves database rows"
    );
    interact(window, cx, |window, cx| window.click("settings-close", cx));
    wait_until(window, cx, "settings dismissed", |window, _| {
        window.try_find("settings-close").is_none()
    });
    interact(window, cx, |window, cx| window.click("place-root", cx));
    wait_until(window, cx, "leave rows", |window, _| {
        window.try_find("discard-leave").is_some()
    });
    interact(window, cx, |window, cx| window.click("discard-leave", cx));
    wait_until(window, cx, "connection", |window, _| {
        window.try_find(fixture.connection_key.clone()).is_some()
    });
    interact(window, cx, |window, cx| {
        window.right_click(fixture.connection_key.clone(), cx)
    });
    wait_until(window, cx, "entry menu", |window, _| {
        window.try_find("popup-menu").is_some()
    });
    interact(window, cx, |window, cx| {
        window.within("popup-menu").click(2_usize, cx)
    });
    wait_until(window, cx, "labels", |window, _| {
        window.try_find("assign-label-analytics").is_some()
    });
    interact(window, cx, |window, cx| {
        window.click("assign-label-analytics", cx)
    });
    wait_until(window, cx, "assignment saved", |_, _| {
        Workspace::open(&fixture.root)
            .expect("workspace")
            .manifest()
            .label_assignments
            .iter()
            .any(|assignment| assignment.labels.contains(&"analytics".into()))
    });
}

#[gpui_kit::test]
fn encrypted_workspaces_autosave_connection_details_and_keep_drafts_out_of_local_state(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let path = fixture.root.with_file_name("private.qwerio");
    let password = Secret::new("workspace password");
    Workspace::create_file(&path, "Private", Some(&password)).expect("encrypted workspace");
    init_app(fixture.services.clone(), cx);
    let (window, _) = open_workbench(Some(path.clone()), cx);
    choose_password(window, cx, password.expose(), false);
    wait_until(window, cx, "private workspace", |window, _| {
        window.try_find("add-connection").is_some()
    });
    interact(window, cx, |window, cx| window.click("add-connection", cx));
    wait_until(window, cx, "connection form", |window, _| {
        window.try_find("connection-engine").is_some()
    });
    choose_option(window, cx, "connection-engine", 1);
    wait_until(window, cx, "SQLite fields", |window, _| {
        window.try_find("connection-file").is_some()
    });
    interact(window, cx, |window, cx| {
        window.click("connection-file", cx);
        window.input(&fixture.database.display().to_string(), cx);
        window.click("save-connection", cx);
    });
    wait_until(window, cx, "connection saved", |window, _| {
        window.try_find("connection-name").is_none()
    });
    let saved = Workspace::open_file(&path, Some(&password)).expect("workspace");
    let connection = saved.manifest().connections[0].id;
    assert_eq!(
        saved
            .stored_connection(connection)
            .expect("portable details")
            .options,
        ConnectionOptions::Sqlite(SqliteOptions::new(&fixture.database))
    );
    assert!(
        fixture
            .services
            .local_state
            .binding(saved.manifest().id, connection)
            .expect("local binding")
            .is_none()
    );
    interact(window, cx, |window, cx| window.click("new-note", cx));
    wait_until(window, cx, "new note", |window, _| {
        label_of(window, "status").is_some_and(|status| status.starts_with("Saved"))
    });
    interact(window, cx, |window, cx| {
        window.input("private draft text", cx)
    });
    wait_until(window, cx, "modified note", |window, _| {
        label_of(window, "status").is_some_and(|status| status.starts_with("Modified"))
    });
    interact(window, cx, |window, cx| {
        window.dispatch_action(Box::new(crate::actions::CloseWorkspace), cx)
    });
    wait_until(window, cx, "closed after draft flush", |window, _| {
        window.try_find("open-workspace").is_some()
    });
    let saved = Workspace::open_file(&path, Some(&password)).expect("workspace");
    let note = saved.manifest().documents[0].id;
    assert_eq!(saved.draft(note), Some("private draft text"));
    assert!(
        fixture
            .services
            .local_state
            .drafts(saved.manifest().id)
            .expect("local drafts")
            .is_empty()
    );
    let bytes = std::fs::read(&path).expect("ciphertext");
    assert!(
        !bytes
            .windows("private draft text".len())
            .any(|window| window == b"private draft text")
    );
}
