//! Resource identities for database objects and catalog containers.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::ConnectionId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    ForeignTable,
}

impl ObjectKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Table => "Table",
            Self::View => "View",
            Self::MaterializedView => "Materialized view",
            Self::ForeignTable => "Foreign table",
        }
    }

    pub fn is_view_like(self) -> bool {
        matches!(self, Self::View | Self::MaterializedView)
    }
}

/// A container inside one connection: the connection root, a database, or a
/// namespace. Adapters decide which levels exist; SQLite has neither databases
/// nor visible namespaces, so its objects live directly under the root.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CatalogPath {
    pub database: Option<String>,
    pub namespace: Option<String>,
}

impl CatalogPath {
    pub fn root() -> Self {
        Self::default()
    }

    pub fn database(database: impl Into<String>) -> Self {
        Self {
            database: Some(database.into()),
            namespace: None,
        }
    }

    pub fn namespace(database: Option<String>, namespace: impl Into<String>) -> Self {
        Self {
            database,
            namespace: Some(namespace.into()),
        }
    }
}

/// A stable logical reference to a table-like object.
///
/// Identity is the connection alias, container, kind, and name. Adapter-native
/// identifiers (OIDs, rowids) may change across restores and are not stored.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObjectRef {
    pub connection: ConnectionId,
    pub database: Option<String>,
    pub namespace: Option<String>,
    pub kind: ObjectKind,
    pub name: String,
}

impl ObjectRef {
    pub fn container(&self) -> CatalogPath {
        CatalogPath {
            database: self.database.clone(),
            namespace: self.namespace.clone(),
        }
    }

    /// The name qualified by its namespace, for display.
    pub fn qualified_name(&self) -> String {
        match &self.namespace {
            Some(namespace) => format!("{namespace}.{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Same object ignoring the kind, which may legitimately change (a table
    /// replaced by a view keeps its associations).
    pub fn same_location_and_name(&self, other: &Self) -> bool {
        self.connection == other.connection
            && self.database == other.database
            && self.namespace == other.namespace
            && self.name == other.name
    }
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.qualified_name())
    }
}

/// Whether the current credentials can use an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntryAccess {
    Accessible,
    /// Visible in the catalog, but the role lacks the privilege to use it.
    NoPrivilege,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CatalogEntryKind {
    Database,
    Namespace { is_system: bool },
    Object(ObjectKind),
}

/// One child returned when listing a catalog container.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub kind: CatalogEntryKind,
    pub name: String,
    pub access: EntryAccess,
    pub comment: Option<String>,
}

impl CatalogEntry {
    /// The container path for a database/namespace entry listed under `parent`.
    pub fn child_path(&self, parent: &CatalogPath) -> Option<CatalogPath> {
        match self.kind {
            CatalogEntryKind::Database => Some(CatalogPath::database(self.name.clone())),
            CatalogEntryKind::Namespace { .. } => Some(CatalogPath::namespace(
                parent.database.clone(),
                self.name.clone(),
            )),
            CatalogEntryKind::Object(_) => None,
        }
    }

    /// The object reference for an object entry listed under `parent`.
    pub fn object_ref(&self, connection: ConnectionId, parent: &CatalogPath) -> Option<ObjectRef> {
        match self.kind {
            CatalogEntryKind::Object(kind) => Some(ObjectRef {
                connection,
                database: parent.database.clone(),
                namespace: parent.namespace.clone(),
                kind,
                name: self.name.clone(),
            }),
            _ => None,
        }
    }
}
