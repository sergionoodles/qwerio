use qwerio_core::connection::{ConnectionOptions, EngineKind, Secret, SqliteOptions};
use qwerio_workspace::document::SaveExpectation;
use qwerio_workspace::file::StoredConnection;
use qwerio_workspace::labels::LabelTarget;
use qwerio_workspace::manifest::Placement;
use qwerio_workspace::{Workspace, WorkspaceError};
use std::path::Path;
use tempfile::TempDir;

#[test]
fn plain_and_encrypted_files_preserve_documents_connections_and_labels() {
    let directory = TempDir::new().expect("directory");
    for password in [None, Some(Secret::new("workspace password"))] {
        let path = directory.path().join(if password.is_some() {
            "encrypted.qwerio"
        } else {
            "plain.qwerio"
        });
        let mut workspace =
            Workspace::create_file(&path, "Work", password.as_ref()).expect("create");
        let connection = workspace
            .add_connection("data", EngineKind::Sqlite)
            .expect("alias");
        let stored = StoredConnection {
            id: connection,
            options: ConnectionOptions::Sqlite(SqliteOptions::new("data.sqlite")),
            password: Some(Secret::new("database secret")),
        };
        workspace
            .save_connection(stored.clone())
            .expect("save connection");
        workspace.define_label("custom").expect("label");
        workspace
            .assign_labels(
                LabelTarget::Connection { connection },
                vec!["prod".into(), "custom".into()],
            )
            .expect("assign");
        let (document, fingerprint) = workspace
            .create_document(Path::new("query.sql"), Placement::Root, "select 'private';")
            .expect("document");
        workspace
            .save_document(
                document,
                "select 'changed';",
                SaveExpectation::Unchanged(fingerprint),
            )
            .expect("save");
        workspace
            .save_draft(document, Some("select 'draft';".into()))
            .expect("draft");
        let reopened = Workspace::open_file(&path, password.as_ref()).expect("open");
        assert_eq!(reopened.manifest(), workspace.manifest());
        assert_eq!(reopened.stored_connection(connection), Some(&stored));
        assert_eq!(
            reopened.load_document(document).expect("load").text,
            "select 'changed';"
        );
        assert_eq!(reopened.draft(document), Some("select 'draft';"));
        assert!(!path.join("query.sql").exists());
        if password.is_some() {
            let bytes = std::fs::read(&path).expect("bytes");
            for secret in [
                "private",
                "changed",
                "draft",
                "database secret",
                "data.sqlite",
                "custom",
            ] {
                assert!(
                    !bytes
                        .windows(secret.len())
                        .any(|window| window == secret.as_bytes())
                );
            }
        }
    }
}

#[test]
fn export_uses_an_independent_password() {
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("source.qwerio");
    let exported = directory.path().join("export.qwerio");
    let original_password = Secret::new("original");
    let export_password = Secret::new("independent");
    let workspace =
        Workspace::create_file(&path, "Work", Some(&original_password)).expect("create");
    workspace
        .export(&exported, Some(&export_password))
        .expect("export");
    assert!(Workspace::open_file(&exported, Some(&original_password)).is_err());
    assert!(Workspace::open_file(&exported, Some(&export_password)).is_ok());
    assert!(Workspace::open_file(&path, Some(&original_password)).is_ok());
    let plain = directory.path().join("plain.qwerio");
    workspace.export(&plain, None).expect("plain export");
    assert!(Workspace::open_file(&plain, None).is_ok());
}

#[test]
fn missing_wrong_password_and_corruption_are_rejected() {
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("work.qwerio");
    Workspace::create_file(&path, "Work", Some(&Secret::new("correct"))).expect("create");
    assert!(matches!(
        Workspace::open_file(&path, None),
        Err(WorkspaceError::PasswordRequired)
    ));
    assert!(matches!(
        Workspace::open_file(&path, Some(&Secret::new("wrong"))),
        Err(WorkspaceError::Authentication)
    ));
    let original = std::fs::read(&path).expect("read");
    for length in [0, 8, 9, 24, 36, original.len() - 1] {
        std::fs::write(&path, &original[..length]).expect("truncate");
        assert!(Workspace::open_file(&path, Some(&Secret::new("correct"))).is_err());
    }
    let mut corrupt = original;
    let last = corrupt.last_mut().expect("last");
    *last ^= 1;
    std::fs::write(&path, corrupt).expect("corrupt");
    assert!(Workspace::open_file(&path, Some(&Secret::new("correct"))).is_err());
}

#[test]
fn failed_autosave_preserves_memory_and_external_file() {
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("work.qwerio");
    let mut workspace = Workspace::create_file(&path, "Work", None).expect("create");
    let mut other = Workspace::open_file(&path, None).expect("open");
    other.rename_workspace("External").expect("external edit");
    let external = std::fs::read(&path).expect("read");
    assert!(matches!(
        workspace.rename_workspace("Lost"),
        Err(WorkspaceError::Conflict { .. })
    ));
    assert_eq!(workspace.manifest().name, "Work");
    assert_eq!(std::fs::read(&path).expect("read"), external);
}

#[test]
fn file_metadata_operations_preserve_assignments_and_validate_labels() {
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("work.qwerio");
    let mut workspace = Workspace::create_file(&path, "Work", None).expect("create");
    assert_eq!(
        workspace.manifest().labels,
        ["prod", "test", "staging", "dev"]
    );
    assert!(workspace.define_label("  ").is_err());
    assert!(workspace.define_label(&"x".repeat(65)).is_err());
    let connection = workspace
        .add_connection("data", EngineKind::Sqlite)
        .expect("alias");
    workspace
        .save_connection(StoredConnection {
            id: connection,
            options: ConnectionOptions::Sqlite(SqliteOptions::new("data.sqlite")),
            password: None,
        })
        .expect("connection");
    let object = qwerio_core::resource::ObjectRef {
        connection,
        database: None,
        namespace: None,
        kind: qwerio_core::resource::ObjectKind::Table,
        name: "orders".into(),
    };
    let target = LabelTarget::Object { object };
    assert!(
        workspace
            .assign_labels(target.clone(), vec!["undefined".into()])
            .is_err()
    );
    workspace
        .assign_labels(target.clone(), vec!["dev".into()])
        .expect("assign object");
    let folder = workspace
        .create_folder("Queries", Placement::Root)
        .expect("folder");
    let (document, _) = workspace
        .create_document(Path::new("old.sql"), Placement::Root, "SELECT 1")
        .expect("document");
    workspace
        .rename_document(document, "new.sql")
        .expect("rename");
    workspace
        .move_entry(
            qwerio_workspace::MovableEntry::Document(document),
            Placement::Folder { folder },
        )
        .expect("move");
    let reopened = Workspace::open_file(&path, None).expect("reopen");
    assert_eq!(reopened.manifest().documents[0].path, "new.sql");
    assert_eq!(reopened.manifest().label_assignments[0].target, target);
    assert_eq!(
        reopened.load_document(document).expect("document").text,
        "SELECT 1"
    );
    workspace
        .remove_connection(connection)
        .expect("remove bound connection");
    assert!(
        Workspace::open_file(&path, None)
            .expect("reopen")
            .stored_connection(connection)
            .is_none()
    );
}

#[test]
fn encrypted_autosave_keeps_encryption_and_refuses_external_changes() {
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("work.qwerio");
    let password = Secret::new("secret");
    let mut workspace = Workspace::create_file(&path, "Work", Some(&password)).expect("create");
    workspace
        .rename_workspace("Sensitive name")
        .expect("autosave");
    let updated = std::fs::read(&path).expect("bytes");
    assert!(
        !updated
            .windows("Sensitive name".len())
            .any(|window| window == b"Sensitive name")
    );
    let mut other = Workspace::open_file(&path, Some(&password)).expect("other");
    other.rename_workspace("External").expect("external save");
    assert!(workspace.rename_workspace("Must not replace").is_err());
    assert_eq!(workspace.manifest().name, "Sensitive name");
    assert_eq!(
        Workspace::open_file(&path, Some(&password))
            .expect("read")
            .manifest()
            .name,
        "External"
    );
}

#[test]
fn legacy_export_preserves_tracked_and_untracked_documents_without_changing_sources() {
    let directory = TempDir::new().expect("directory");
    let mut legacy = Workspace::create(directory.path(), "Legacy").expect("legacy");
    legacy
        .create_document(Path::new("tracked.md"), Placement::Root, "Tracked note")
        .expect("tracked document");
    let loose = directory.path().join("loose.sql");
    std::fs::write(&loose, "SELECT 42;").expect("untracked document");
    let exported = directory.path().join("export.qwerio");
    legacy.export(&exported, None).expect("export");
    let imported = Workspace::open_file(&exported, None).expect("import");
    let document = imported
        .manifest()
        .document_by_path("loose.sql")
        .expect("untracked document preserved");
    assert_eq!(
        imported.load_document(document.id).expect("contents").text,
        "SELECT 42;"
    );
    assert_eq!(imported.manifest().documents.len(), 2);
    assert_eq!(
        std::fs::read_to_string(&loose).expect("original"),
        "SELECT 42;"
    );
    assert_eq!(
        Workspace::open(directory.path())
            .expect("legacy")
            .manifest()
            .documents
            .len(),
        1
    );
}

#[test]
fn deleting_an_embedded_document_removes_its_body_draft_and_labels_from_exports() {
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("private.qwerio");
    let password = Secret::new("workspace password");
    let mut workspace =
        Workspace::create_file(&path, "Private", Some(&password)).expect("workspace");
    let (document, _) = workspace
        .create_document(
            Path::new("private.md"),
            Placement::Root,
            "deleted document marker",
        )
        .expect("document");
    workspace
        .save_draft(document, Some("deleted draft marker".into()))
        .expect("draft");
    workspace
        .assign_labels(LabelTarget::Document { document }, vec!["prod".into()])
        .expect("labels");
    workspace.remove_document(document, true).expect("delete");
    let reopened = Workspace::open_file(&path, Some(&password)).expect("reopen");
    assert!(reopened.load_document(document).is_err());
    assert!(reopened.draft(document).is_none());
    assert!(reopened.manifest().label_assignments.is_empty());
    let export = directory.path().join("export.qwerio");
    reopened.export(&export, None).expect("export");
    let bytes = std::fs::read(export).expect("export bytes");
    for marker in ["deleted document marker", "deleted draft marker"] {
        assert!(
            !bytes
                .windows(marker.len())
                .any(|window| window == marker.as_bytes())
        );
    }
}
