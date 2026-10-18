//! Workspace behavior through its public API on temporary folders.

use std::fs;
use std::path::Path;

use qwerio_core::connection::EngineKind;
use qwerio_core::resource::{CatalogPath, ObjectKind, ObjectRef};
use qwerio_workspace::document::SaveExpectation;
use qwerio_workspace::manifest::{ExecutionBinding, Placement};
use qwerio_workspace::{MovableEntry, Workspace, WorkspaceError};
use tempfile::TempDir;

fn new_workspace() -> (TempDir, Workspace) {
    let directory = TempDir::new().expect("temporary directory");
    let workspace = Workspace::create(directory.path(), "Sales").expect("create workspace");
    (directory, workspace)
}

fn table(connection: qwerio_core::ids::ConnectionId, name: &str) -> ObjectRef {
    ObjectRef {
        connection,
        database: Some("sales".to_owned()),
        namespace: Some("public".to_owned()),
        kind: ObjectKind::Table,
        name: name.to_owned(),
    }
}

#[test]
fn workspaces_survive_reopening() {
    let (directory, mut workspace) = new_workspace();
    let connection = workspace
        .add_connection("Warehouse", EngineKind::Postgres)
        .expect("connection");
    let placement = Placement::container(
        connection,
        &CatalogPath::namespace(Some("sales".to_owned()), "public"),
    );
    let (document, _) = workspace
        .create_document(Path::new("queries/top.sql"), placement.clone(), "SELECT 1")
        .expect("document");

    let reopened = Workspace::open(directory.path()).expect("reopen");

    assert_eq!(reopened.manifest(), workspace.manifest());
    assert_eq!(
        reopened
            .manifest()
            .document(document)
            .map(|entry| &entry.placement),
        Some(&placement)
    );
    assert_eq!(
        reopened
            .load_document(document)
            .map(|loaded| loaded.text)
            .ok()
            .as_deref(),
        Some("SELECT 1")
    );
}

#[test]
fn a_document_can_reference_several_tables_and_workspace_queries_need_none() {
    let (_directory, mut workspace) = new_workspace();
    let connection = workspace
        .add_connection("Warehouse", EngineKind::Sqlite)
        .expect("connection");
    let (report, _) = workspace
        .create_document(Path::new("report.md"), Placement::Root, "# Report")
        .expect("report");
    let (scratch, _) = workspace
        .create_document(Path::new("scratch.sql"), Placement::Root, "SELECT 1")
        .expect("scratch");

    workspace
        .associate(report, table(connection, "orders"))
        .expect("associate orders");
    workspace
        .associate(report, table(connection, "lines"))
        .expect("associate lines");
    workspace
        .associate(report, table(connection, "orders"))
        .expect("duplicate is ignored");
    workspace
        .set_execution_binding(
            scratch,
            Some(ExecutionBinding {
                connection,
                database: None,
            }),
        )
        .expect("binding");

    let manifest = workspace.manifest();
    assert_eq!(
        manifest
            .document(report)
            .map(|entry| entry.associations.len()),
        Some(2)
    );
    assert!(
        manifest
            .document(scratch)
            .is_some_and(|entry| entry.associations.is_empty())
    );
    assert_eq!(
        manifest
            .documents_for_object(&table(connection, "lines"))
            .len(),
        1
    );
}

#[test]
fn documents_cannot_escape_the_workspace() {
    let (_directory, mut workspace) = new_workspace();

    for path in ["../outside.sql", "/tmp/absolute.sql", "a/../../b.sql"] {
        let result = workspace.create_document(Path::new(path), Placement::Root, "");
        assert!(
            matches!(result, Err(WorkspaceError::OutsideWorkspace(_))),
            "{path}"
        );
    }

    let unsupported = workspace.create_document(Path::new("image.png"), Placement::Root, "");
    assert!(matches!(
        unsupported,
        Err(WorkspaceError::UnsupportedDocument(_))
    ));
}

#[test]
fn external_edits_produce_conflicts_instead_of_overwrites() {
    let (directory, mut workspace) = new_workspace();
    let (document, loaded) = workspace
        .create_document(Path::new("q.sql"), Placement::Root, "SELECT 1")
        .expect("document");

    fs::write(directory.path().join("q.sql"), "SELECT 'external'").expect("external edit");
    let result = workspace.save_document(document, "SELECT 2", SaveExpectation::Unchanged(loaded));

    assert!(matches!(result, Err(WorkspaceError::Conflict { .. })));
    assert_eq!(
        fs::read_to_string(directory.path().join("q.sql"))
            .ok()
            .as_deref(),
        Some("SELECT 'external'")
    );

    workspace
        .save_document(document, "SELECT 2", SaveExpectation::Overwrite)
        .expect("explicit overwrite");
}

#[test]
fn manifest_changes_from_other_programs_are_not_overwritten() {
    let (directory, mut workspace) = new_workspace();
    let manifest_path = workspace.manifest_path();
    let edited = fs::read_to_string(&manifest_path)
        .expect("read manifest")
        .replace("name = \"Sales\"", "name = \"Renamed elsewhere\"");
    fs::write(&manifest_path, edited).expect("external edit");

    let result = workspace.add_connection("Local", EngineKind::Sqlite);
    assert!(matches!(result, Err(WorkspaceError::Conflict { .. })));

    workspace.reload_manifest().expect("reload");
    assert_eq!(workspace.manifest().name, "Renamed elsewhere");
    workspace
        .add_connection("Local", EngineKind::Sqlite)
        .expect("add after reload");
    assert!(Workspace::open(directory.path()).is_ok());
}

#[test]
fn folders_cannot_be_moved_into_themselves() {
    let (_directory, mut workspace) = new_workspace();
    let outer = workspace
        .create_folder("Outer", Placement::Root)
        .expect("outer");
    let inner = workspace
        .create_folder("Inner", Placement::Folder { folder: outer })
        .expect("inner");

    let into_child = workspace.move_entry(
        MovableEntry::Folder(outer),
        Placement::Folder { folder: inner },
    );
    let into_self = workspace.move_entry(
        MovableEntry::Folder(outer),
        Placement::Folder { folder: outer },
    );

    assert!(matches!(into_child, Err(WorkspaceError::FolderCycle)));
    assert!(matches!(into_self, Err(WorkspaceError::FolderCycle)));
}

#[test]
fn removing_a_folder_keeps_its_contents_in_the_parent() {
    let (_directory, mut workspace) = new_workspace();
    let folder = workspace
        .create_folder("Reports", Placement::Root)
        .expect("folder");
    let (document, _) = workspace
        .create_document(Path::new("r.md"), Placement::Folder { folder }, "")
        .expect("document");

    workspace.remove_folder(folder).expect("remove folder");

    assert_eq!(
        workspace
            .manifest()
            .document(document)
            .map(|entry| &entry.placement),
        Some(&Placement::Root)
    );
}

#[test]
fn moving_entries_changes_placement_but_not_files() {
    let (directory, mut workspace) = new_workspace();
    let folder = workspace
        .create_folder("Reports", Placement::Root)
        .expect("folder");
    let (document, _) = workspace
        .create_document(Path::new("q.sql"), Placement::Root, "SELECT 1")
        .expect("document");

    workspace
        .move_entry(
            MovableEntry::Document(document),
            Placement::Folder { folder },
        )
        .expect("move");

    assert!(directory.path().join("q.sql").is_file());
    assert_eq!(
        workspace
            .manifest()
            .document(document)
            .map(|entry| entry.path.as_str()),
        Some("q.sql")
    );
}

#[test]
fn renaming_a_document_renames_its_file() {
    let (directory, mut workspace) = new_workspace();
    let (document, _) = workspace
        .create_document(Path::new("queries/a.sql"), Placement::Root, "SELECT 1")
        .expect("document");

    workspace
        .rename_document(document, "b.sql")
        .expect("rename");

    assert!(!directory.path().join("queries/a.sql").exists());
    assert!(directory.path().join("queries/b.sql").is_file());
    assert_eq!(
        workspace
            .manifest()
            .document(document)
            .map(|entry| entry.path.as_str()),
        Some("queries/b.sql")
    );

    workspace
        .create_document(Path::new("queries/c.sql"), Placement::Root, "")
        .expect("second document");
    assert!(matches!(
        workspace.rename_document(document, "c.sql"),
        Err(WorkspaceError::AlreadyExists(_))
    ));
}

#[test]
fn untracked_documents_are_found_and_can_be_registered() {
    let (directory, mut workspace) = new_workspace();
    fs::create_dir_all(directory.path().join("notes")).expect("notes folder");
    fs::create_dir_all(directory.path().join(".hidden")).expect("hidden folder");
    fs::write(directory.path().join("notes/todo.md"), "- [ ] x").expect("todo");
    fs::write(directory.path().join(".hidden/secret.sql"), "").expect("hidden");
    fs::write(directory.path().join("data.csv"), "").expect("csv");

    assert_eq!(
        workspace.untracked_documents().expect("scan"),
        vec!["notes/todo.md"]
    );

    let id = workspace
        .register_document(Path::new("notes/todo.md"), Placement::Root)
        .expect("register");
    assert!(workspace.untracked_documents().expect("rescan").is_empty());
    assert_eq!(
        workspace
            .register_document(Path::new("notes/todo.md"), Placement::Root)
            .ok(),
        Some(id)
    );
}

#[test]
fn missing_objects_remain_repairable_associations() {
    let (_directory, mut workspace) = new_workspace();
    let connection = workspace
        .add_connection("Warehouse", EngineKind::Postgres)
        .expect("connection");
    let (document, _) = workspace
        .create_document(Path::new("q.sql"), Placement::Root, "")
        .expect("document");
    workspace
        .associate(document, table(connection, "orders"))
        .expect("associate");

    let unresolved = workspace.unresolved_associations(|object| object.name != "orders");
    assert_eq!(unresolved, vec![(document, table(connection, "orders"))]);

    workspace
        .replace_association(
            document,
            &table(connection, "orders"),
            table(connection, "orders_2024"),
        )
        .expect("repair");
    assert!(
        workspace
            .unresolved_associations(|object| object.name != "orders")
            .is_empty()
    );
}

#[test]
fn removing_a_connection_keeps_documents_and_their_associations() {
    let (_directory, mut workspace) = new_workspace();
    let connection = workspace
        .add_connection("Warehouse", EngineKind::Postgres)
        .expect("connection");
    let placement = Placement::container(connection, &CatalogPath::root());
    let (document, _) = workspace
        .create_document(Path::new("q.sql"), placement, "")
        .expect("document");
    workspace
        .associate(document, table(connection, "orders"))
        .expect("associate");

    workspace
        .remove_connection(connection)
        .expect("remove connection");

    let entry = workspace
        .manifest()
        .document(document)
        .expect("document kept");
    assert_eq!(entry.placement, Placement::Root);
    assert_eq!(entry.associations.len(), 1);
}

#[test]
fn deleted_document_files_can_be_relinked() {
    let (directory, mut workspace) = new_workspace();
    let (document, _) = workspace
        .create_document(Path::new("old.sql"), Placement::Root, "SELECT 1")
        .expect("document");
    fs::rename(
        directory.path().join("old.sql"),
        directory.path().join("moved.sql"),
    )
    .expect("external move");

    assert!(workspace.load_document(document).is_err());
    workspace
        .relink_document(document, Path::new("moved.sql"))
        .expect("relink");
    assert_eq!(
        workspace
            .load_document(document)
            .map(|loaded| loaded.text)
            .ok()
            .as_deref(),
        Some("SELECT 1")
    );
}

#[test]
fn creating_a_workspace_twice_is_refused() {
    let (directory, _workspace) = new_workspace();

    assert!(matches!(
        Workspace::create(directory.path(), "Again"),
        Err(WorkspaceError::AlreadyExists(_))
    ));
}
