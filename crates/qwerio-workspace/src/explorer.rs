//! The mixed explorer model: virtual database resources and real documents
//! resolved into one list of entries per location, plus structural
//! breadcrumbs.

use std::collections::HashSet;

use qwerio_core::connection::EngineKind;
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::{
    CatalogEntry, CatalogEntryKind, CatalogPath, EntryAccess, ObjectKind, ObjectRef,
};
use serde::{Deserialize, Serialize};

use crate::document::DocumentKind;
use crate::manifest::{DocumentEntry, DocumentId, FolderId, Manifest, Placement};
use crate::paths::from_portable;
use crate::workspace::MovableEntry;

/// Identifies a temporary query that is not saved as a document yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScratchId(uuid::Uuid);

impl ScratchId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for ScratchId {
    fn default() -> Self {
        Self::new()
    }
}

/// A place the user can navigate to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Location {
    Root,
    Folder(FolderId),
    /// A connection's root, one of its databases, or a namespace.
    Container {
        connection: ConnectionId,
        path: CatalogPath,
    },
    Object(ObjectRef),
    Document(DocumentId),
    /// A temporary query tab; it exists only until saved or closed.
    Scratch(ScratchId),
}

impl Location {
    pub fn connection_root(connection: ConnectionId) -> Self {
        Self::Container {
            connection,
            path: CatalogPath::root(),
        }
    }

    pub fn from_placement(placement: &Placement) -> Self {
        match placement {
            Placement::Root => Self::Root,
            Placement::Folder { folder } => Self::Folder(*folder),
            Placement::Container {
                connection,
                database,
                namespace,
            } => Self::Container {
                connection: *connection,
                path: CatalogPath {
                    database: database.clone(),
                    namespace: namespace.clone(),
                },
            },
        }
    }

    /// The placement that new entries created at this location receive.
    pub fn placement(&self) -> Option<Placement> {
        match self {
            Self::Root => Some(Placement::Root),
            Self::Folder(folder) => Some(Placement::Folder { folder: *folder }),
            Self::Container { connection, path } => Some(Placement::container(*connection, path)),
            Self::Object(_) | Self::Document(_) | Self::Scratch(_) => None,
        }
    }

    /// Whether this location lists entries (rather than opening content).
    pub fn is_container(&self) -> bool {
        matches!(self, Self::Root | Self::Folder(_) | Self::Container { .. })
    }
}

/// The location whose entries surround `location`: itself for containers,
/// otherwise where the object or document lives.
pub fn enclosing_container(manifest: &Manifest, location: &Location) -> Location {
    match location {
        Location::Object(object) => Location::Container {
            connection: object.connection,
            path: object.container(),
        },
        Location::Document(id) => manifest.document(*id).map_or(Location::Root, |document| {
            Location::from_placement(&document.placement)
        }),
        Location::Scratch(_) => Location::Root,
        container => container.clone(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Folder,
    Connection(EngineKind),
    Database,
    Namespace { is_system: bool },
    Object(ObjectKind),
    Document(DocumentKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryStatus {
    Ready,
    /// Listed by the catalog, but the role cannot use it.
    NoPrivilege,
    /// The document's file is missing; the entry can be relinked or removed.
    MissingFile,
    /// A supported file in the folder that the manifest does not list yet.
    Untracked,
    /// The connection alias has no endpoint or file on this machine.
    Unbound,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntryTarget {
    Open(Location),
    /// Registering the file makes it openable.
    Untracked(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExplorerEntry {
    /// Stable identity for views and selection.
    pub key: String,
    pub name: String,
    pub kind: EntryKind,
    pub detail: Option<String>,
    pub status: EntryStatus,
    pub target: EntryTarget,
    pub movable: Option<MovableEntry>,
}

pub struct ExplorerInput<'a> {
    pub manifest: &'a Manifest,
    pub location: &'a Location,
    /// The catalog children of a container location, once loaded.
    pub catalog: Option<&'a [CatalogEntry]>,
    /// Untracked documents, shown at the workspace root.
    pub untracked: &'a [String],
    pub missing_documents: &'a HashSet<DocumentId>,
    pub unbound_connections: &'a HashSet<ConnectionId>,
}

/// Group order: folders, connections, databases, namespaces, objects, documents.
fn group_rank(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::Folder => 0,
        EntryKind::Connection(_) => 1,
        EntryKind::Database => 2,
        EntryKind::Namespace { is_system: false } => 3,
        EntryKind::Namespace { is_system: true } => 4,
        EntryKind::Object(_) => 5,
        EntryKind::Document(_) => 6,
    }
}

pub fn list_entries(input: &ExplorerInput<'_>) -> Vec<ExplorerEntry> {
    let Some(placement) = input.location.placement() else {
        return Vec::new();
    };

    let mut entries: Vec<ExplorerEntry> = Vec::new();

    entries.extend(
        input
            .manifest
            .folders
            .iter()
            .filter(|folder| folder.placement == placement)
            .map(|folder| ExplorerEntry {
                key: format!("folder:{}", folder.id),
                name: folder.name.clone(),
                kind: EntryKind::Folder,
                detail: None,
                status: EntryStatus::Ready,
                target: EntryTarget::Open(Location::Folder(folder.id)),
                movable: Some(MovableEntry::Folder(folder.id)),
            }),
    );

    if *input.location == Location::Root {
        entries.extend(input.manifest.connections.iter().map(|connection| {
            let status = if input.unbound_connections.contains(&connection.id) {
                EntryStatus::Unbound
            } else {
                EntryStatus::Ready
            };

            ExplorerEntry {
                key: format!("connection:{}", connection.id),
                name: connection.name.clone(),
                kind: EntryKind::Connection(connection.engine),
                detail: Some(connection.engine.display_name().to_owned()),
                status,
                target: EntryTarget::Open(Location::connection_root(connection.id)),
                movable: None,
            }
        }));
    }

    if let (Location::Container { connection, path }, Some(catalog)) =
        (input.location, input.catalog)
    {
        entries.extend(
            catalog
                .iter()
                .map(|entry| catalog_entry(*connection, path, entry)),
        );
    }

    entries.extend(
        input
            .manifest
            .documents
            .iter()
            .filter(|document| document.placement == placement)
            .map(|document| document_entry(input, document)),
    );

    entries.sort_by(|left, right| {
        group_rank(left.kind)
            .cmp(&group_rank(right.kind))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });

    if *input.location == Location::Root {
        entries.extend(
            input
                .untracked
                .iter()
                .filter_map(|path| untracked_entry(path)),
        );
    }

    entries
}

fn catalog_entry(
    connection: ConnectionId,
    parent: &CatalogPath,
    entry: &CatalogEntry,
) -> ExplorerEntry {
    let status = match entry.access {
        EntryAccess::Accessible => EntryStatus::Ready,
        EntryAccess::NoPrivilege => EntryStatus::NoPrivilege,
    };

    let (kind, target, detail) = match entry.kind {
        CatalogEntryKind::Database => (
            EntryKind::Database,
            Location::Container {
                connection,
                path: CatalogPath::database(entry.name.clone()),
            },
            Some("Database".to_owned()),
        ),
        CatalogEntryKind::Namespace { is_system } => (
            EntryKind::Namespace { is_system },
            Location::Container {
                connection,
                path: CatalogPath::namespace(parent.database.clone(), entry.name.clone()),
            },
            Some(if is_system { "System schema" } else { "Schema" }.to_owned()),
        ),
        CatalogEntryKind::Object(object_kind) => (
            EntryKind::Object(object_kind),
            Location::Object(ObjectRef {
                connection,
                database: parent.database.clone(),
                namespace: parent.namespace.clone(),
                kind: object_kind,
                name: entry.name.clone(),
            }),
            Some(object_kind.display_name().to_owned()),
        ),
    };

    let detail = match &entry.comment {
        Some(comment) if !comment.is_empty() => Some(comment.clone()),
        _ => detail,
    };

    ExplorerEntry {
        key: format!("catalog:{connection}:{target:?}"),
        name: entry.name.clone(),
        kind,
        detail,
        status,
        target: EntryTarget::Open(target),
        movable: None,
    }
}

fn document_entry(input: &ExplorerInput<'_>, document: &DocumentEntry) -> ExplorerEntry {
    let kind =
        DocumentKind::from_path(&from_portable(&document.path)).unwrap_or(DocumentKind::Text);
    let status = if input.missing_documents.contains(&document.id) {
        EntryStatus::MissingFile
    } else {
        EntryStatus::Ready
    };

    let associations: Vec<&str> = document
        .associations
        .iter()
        .map(|object| object.name.as_str())
        .collect();
    let detail = if associations.is_empty() {
        kind.display_name().to_owned()
    } else {
        format!("{} · {}", kind.display_name(), associations.join(", "))
    };

    ExplorerEntry {
        key: format!("document:{}", document.id),
        name: document.file_name().to_owned(),
        kind: EntryKind::Document(kind),
        detail: Some(detail),
        status,
        target: EntryTarget::Open(Location::Document(document.id)),
        movable: Some(MovableEntry::Document(document.id)),
    }
}

fn untracked_entry(path: &str) -> Option<ExplorerEntry> {
    let kind = DocumentKind::from_path(&from_portable(path))?;

    Some(ExplorerEntry {
        key: format!("untracked:{path}"),
        name: path.to_owned(),
        kind: EntryKind::Document(kind),
        detail: Some(format!("{} · not in workspace", kind.display_name())),
        status: EntryStatus::Untracked,
        target: EntryTarget::Untracked(path.to_owned()),
        movable: None,
    })
}

/// Keeps entries whose name contains `query`, ignoring case. This filters the
/// visible explorer entries; it never filters records inside a table.
pub fn filter_entries(entries: &[ExplorerEntry], query: &str) -> Vec<ExplorerEntry> {
    let query = query.trim().to_lowercase();

    if query.is_empty() {
        return entries.to_vec();
    }

    entries
        .iter()
        .filter(|entry| entry.name.to_lowercase().contains(&query))
        .cloned()
        .collect()
}

/// One step of the structural path to a location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub location: Location,
}

/// The structural path from the workspace root to `location`. Unlike
/// back/forward history, it does not depend on how the user got there.
pub fn breadcrumbs(manifest: &Manifest, location: &Location) -> Vec<Crumb> {
    let mut crumbs = vec![Crumb {
        label: manifest.name.clone(),
        location: Location::Root,
    }];

    match location {
        Location::Root => {}
        Location::Folder(folder) => push_folder_chain(manifest, *folder, &mut crumbs),
        Location::Container { connection, path } => {
            push_container(manifest, *connection, path, &mut crumbs)
        }
        Location::Object(object) => {
            push_container(
                manifest,
                object.connection,
                &object.container(),
                &mut crumbs,
            );
            crumbs.push(Crumb {
                label: object.name.clone(),
                location: location.clone(),
            });
        }
        Location::Scratch(_) => crumbs.push(Crumb {
            label: "Untitled query".to_owned(),
            location: location.clone(),
        }),
        Location::Document(id) => {
            let Some(document) = manifest.document(*id) else {
                return crumbs;
            };

            match &document.placement {
                Placement::Root => {}
                Placement::Folder { folder } => push_folder_chain(manifest, *folder, &mut crumbs),
                Placement::Container {
                    connection,
                    database,
                    namespace,
                } => {
                    let path = CatalogPath {
                        database: database.clone(),
                        namespace: namespace.clone(),
                    };
                    push_container(manifest, *connection, &path, &mut crumbs);
                }
            }

            crumbs.push(Crumb {
                label: document.file_name().to_owned(),
                location: location.clone(),
            });
        }
    }

    crumbs
}

fn push_container(
    manifest: &Manifest,
    connection: ConnectionId,
    path: &CatalogPath,
    crumbs: &mut Vec<Crumb>,
) {
    let name = manifest.connection(connection).map_or_else(
        || "Unknown connection".to_owned(),
        |alias| alias.name.clone(),
    );

    crumbs.push(Crumb {
        label: name,
        location: Location::connection_root(connection),
    });

    if let Some(database) = &path.database {
        crumbs.push(Crumb {
            label: database.clone(),
            location: Location::Container {
                connection,
                path: CatalogPath::database(database.clone()),
            },
        });
    }

    if let Some(namespace) = &path.namespace {
        crumbs.push(Crumb {
            label: namespace.clone(),
            location: Location::Container {
                connection,
                path: path.clone(),
            },
        });
    }
}

fn push_folder_chain(manifest: &Manifest, folder: FolderId, crumbs: &mut Vec<Crumb>) {
    let mut chain = Vec::new();
    let mut current = Some(folder);
    let mut container = None;

    while let Some(id) = current {
        let Some(entry) = manifest.folder(id) else {
            break;
        };

        if chain.len() > manifest.folders.len() {
            break;
        }

        chain.push(Crumb {
            label: entry.name.clone(),
            location: Location::Folder(id),
        });

        current = match &entry.placement {
            Placement::Folder { folder } => Some(*folder),
            Placement::Container {
                connection,
                database,
                namespace,
            } => {
                container = Some((
                    *connection,
                    CatalogPath {
                        database: database.clone(),
                        namespace: namespace.clone(),
                    },
                ));
                None
            }
            Placement::Root => None,
        };
    }

    if let Some((connection, path)) = container {
        push_container(manifest, connection, &path, crumbs);
    }

    crumbs.extend(chain.into_iter().rev());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{ConnectionAlias, FolderEntry};

    struct Fixture {
        manifest: Manifest,
        connection: ConnectionId,
        folder: FolderId,
        nested: FolderId,
        query: DocumentId,
    }

    fn fixture() -> Fixture {
        let connection = ConnectionId::new();
        let folder = FolderId::new();
        let nested = FolderId::new();
        let query = DocumentId::new();
        let mut manifest = Manifest::new("Sales");

        manifest.connections.push(ConnectionAlias {
            id: connection,
            name: "Warehouse".to_owned(),
            engine: EngineKind::Postgres,
        });
        manifest.folders.push(FolderEntry {
            id: folder,
            name: "Reports".to_owned(),
            placement: Placement::Container {
                connection,
                database: Some("sales".to_owned()),
                namespace: Some("public".to_owned()),
            },
        });
        manifest.folders.push(FolderEntry {
            id: nested,
            name: "Monthly".to_owned(),
            placement: Placement::Folder { folder },
        });
        manifest.documents.push(DocumentEntry {
            id: query,
            path: "queries/top.sql".to_owned(),
            placement: Placement::Container {
                connection,
                database: Some("sales".to_owned()),
                namespace: Some("public".to_owned()),
            },
            associations: Vec::new(),
            execution: None,
        });

        Fixture {
            manifest,
            connection,
            folder,
            nested,
            query,
        }
    }

    fn table(name: &str) -> CatalogEntry {
        CatalogEntry {
            kind: CatalogEntryKind::Object(ObjectKind::Table),
            name: name.to_owned(),
            access: EntryAccess::Accessible,
            comment: None,
        }
    }

    #[test]
    fn namespaces_mix_tables_folders_and_documents() {
        let fixture = fixture();
        let location = Location::Container {
            connection: fixture.connection,
            path: CatalogPath::namespace(Some("sales".to_owned()), "public"),
        };
        let catalog = [table("orders"), table("customers")];
        let empty = HashSet::new();
        let entries = list_entries(&ExplorerInput {
            manifest: &fixture.manifest,
            location: &location,
            catalog: Some(&catalog),
            untracked: &[],
            missing_documents: &empty,
            unbound_connections: &HashSet::new(),
        });

        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["Reports", "customers", "orders", "top.sql"]);
        assert_eq!(
            entries[2].target,
            EntryTarget::Open(Location::Object(ObjectRef {
                connection: fixture.connection,
                database: Some("sales".to_owned()),
                namespace: Some("public".to_owned()),
                kind: ObjectKind::Table,
                name: "orders".to_owned(),
            }))
        );
    }

    #[test]
    fn root_lists_connections_and_untracked_files_with_status() {
        let fixture = fixture();
        let missing = HashSet::new();
        let unbound = HashSet::from([fixture.connection]);
        let untracked = vec!["notes/todo.md".to_owned(), "data.csv".to_owned()];
        let entries = list_entries(&ExplorerInput {
            manifest: &fixture.manifest,
            location: &Location::Root,
            catalog: None,
            untracked: &untracked,
            missing_documents: &missing,
            unbound_connections: &unbound,
        });

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].status, EntryStatus::Unbound);
        assert_eq!(entries[1].status, EntryStatus::Untracked);
        assert_eq!(entries[1].name, "notes/todo.md");
    }

    #[test]
    fn missing_files_are_flagged() {
        let fixture = fixture();
        let location = Location::Container {
            connection: fixture.connection,
            path: CatalogPath::namespace(Some("sales".to_owned()), "public"),
        };
        let missing = HashSet::from([fixture.query]);
        let entries = list_entries(&ExplorerInput {
            manifest: &fixture.manifest,
            location: &location,
            catalog: None,
            untracked: &[],
            missing_documents: &missing,
            unbound_connections: &HashSet::new(),
        });

        let document = entries.iter().find(|entry| entry.name == "top.sql");
        assert_eq!(
            document.map(|entry| entry.status),
            Some(EntryStatus::MissingFile)
        );
    }

    #[test]
    fn breadcrumbs_follow_structure_through_containers_and_folders() {
        let fixture = fixture();
        let crumbs = breadcrumbs(&fixture.manifest, &Location::Folder(fixture.nested));
        let labels: Vec<&str> = crumbs.iter().map(|crumb| crumb.label.as_str()).collect();

        assert_eq!(
            labels,
            vec![
                "Sales",
                "Warehouse",
                "sales",
                "public",
                "Reports",
                "Monthly"
            ]
        );
        assert_eq!(crumbs[4].location, Location::Folder(fixture.folder));
    }

    #[test]
    fn document_breadcrumbs_use_placement_not_file_path() {
        let fixture = fixture();
        let crumbs = breadcrumbs(&fixture.manifest, &Location::Document(fixture.query));
        let labels: Vec<&str> = crumbs.iter().map(|crumb| crumb.label.as_str()).collect();

        assert_eq!(
            labels,
            vec!["Sales", "Warehouse", "sales", "public", "top.sql"]
        );
    }

    #[test]
    fn filtering_matches_names_ignoring_case() {
        let fixture = fixture();
        let location = Location::Container {
            connection: fixture.connection,
            path: CatalogPath::namespace(Some("sales".to_owned()), "public"),
        };
        let catalog = [table("Orders"), table("customers")];
        let entries = list_entries(&ExplorerInput {
            manifest: &fixture.manifest,
            location: &location,
            catalog: Some(&catalog),
            untracked: &[],
            missing_documents: &HashSet::new(),
            unbound_connections: &HashSet::new(),
        });

        let filtered = filter_entries(&entries, "ORD");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "Orders");
    }
}
