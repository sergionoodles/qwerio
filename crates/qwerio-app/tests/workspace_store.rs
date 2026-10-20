use std::path::Path;
use std::sync::Arc;

use qwerio_app::AppServices;
use qwerio_app::credentials::MemoryCredentialStore;
use qwerio_app::local_state::{Draft, LocalState};
use qwerio_app::registry::DriverRegistry;
use qwerio_app::workspace_store::WorkspaceStore;
use qwerio_core::connection::{ConnectionOptions, EngineKind, Secret, SqliteOptions};
use qwerio_workspace::Workspace;
use qwerio_workspace::document::SaveExpectation;
use qwerio_workspace::manifest::Placement;
use tempfile::TempDir;

fn services() -> Arc<AppServices> {
    Arc::new(AppServices::new(
        DriverRegistry::new(),
        LocalState::in_memory().expect("local state"),
        Arc::new(MemoryCredentialStore::default()),
    ))
}

#[test]
fn persistence_operations_use_the_workspace_backend() {
    let directory = TempDir::new().expect("directory");
    for is_file in [false, true] {
        let password = Secret::new("workspace password");
        let workspace = if is_file {
            Workspace::create_file(
                &directory.path().join("portable.qwerio"),
                "Portable",
                Some(&password),
            )
        } else {
            Workspace::create(directory.path(), "Legacy")
        }
        .expect("workspace");
        let services = services();
        let mut store = WorkspaceStore::new(workspace, services.clone());
        let connection = store
            .update(|workspace| workspace.add_connection("Data", EngineKind::Sqlite))
            .expect("alias");
        let (document, fingerprint) = store
            .update(|workspace| {
                workspace.create_document(Path::new("query.sql"), Placement::Root, "SELECT 1;")
            })
            .expect("document");
        store
            .bind_connection(
                connection,
                ConnectionOptions::Sqlite(SqliteOptions::new("data.sqlite")),
                Some(Secret::new("database password")),
            )
            .expect("bind");
        store
            .save_draft(Draft {
                document,
                text: "SELECT 2;".into(),
                base: Some(fingerprint),
            })
            .expect("draft");
        store.save_ui_state(&vec!["query.sql"]).expect("layout");

        assert!(
            store
                .bound_connections()
                .expect("bound")
                .contains(&connection)
        );
        assert_eq!(
            store
                .resolve_connection(connection)
                .expect("resolve")
                .credentials
                .password
                .as_ref()
                .map(Secret::expose),
            Some("database password")
        );
        assert_eq!(
            store.ui_state::<Vec<String>>().expect("layout"),
            Some(vec!["query.sql".into()])
        );
        assert_eq!(
            store
                .load_document(document, true)
                .expect("load")
                .1
                .expect("draft")
                .text,
            "SELECT 2;"
        );
        let local_binding = services
            .local_state
            .binding(store.workspace().manifest().id, connection)
            .expect("local binding");
        assert_eq!(local_binding.is_none(), is_file);
        assert_eq!(
            services
                .local_state
                .drafts(store.workspace().manifest().id)
                .expect("local drafts")
                .is_empty(),
            is_file
        );

        let saved = store
            .save_document(
                document,
                "SELECT 3;",
                SaveExpectation::Unchanged(fingerprint),
            )
            .expect("save");
        assert!(saved.draft_error.is_none());
        assert!(
            store
                .load_document(document, true)
                .expect("load")
                .1
                .is_none()
        );
        assert_eq!(
            store.load_document(document, false).expect("load").0.text,
            "SELECT 3;"
        );
    }
}

#[test]
fn a_failed_workspace_write_preserves_the_saved_document_and_draft() {
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("portable.qwerio");
    let mut store = WorkspaceStore::new(
        Workspace::create_file(&path, "Portable", None).expect("workspace"),
        services(),
    );
    let (document, fingerprint) = store
        .update(|workspace| {
            workspace.create_document(Path::new("note.md"), Placement::Root, "Saved")
        })
        .expect("document");
    store
        .save_draft(Draft {
            document,
            text: "Draft".into(),
            base: Some(fingerprint),
        })
        .expect("draft");
    Workspace::open_file(&path, None)
        .expect("external open")
        .rename_workspace("External")
        .expect("external edit");
    assert!(
        store
            .save_document(document, "New", SaveExpectation::Unchanged(fingerprint))
            .is_err()
    );
    let (loaded, draft) = store.load_document(document, true).expect("load");
    assert_eq!(loaded.text, "Saved");
    assert_eq!(draft.expect("draft").text, "Draft");
}
