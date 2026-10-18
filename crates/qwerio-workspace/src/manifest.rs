//! The versioned, shareable workspace manifest (`.qwerio/workspace.toml`).
//!
//! It holds logical connection aliases, documents with relative paths, virtual
//! placement, and object associations. Endpoints, file paths of SQLite
//! databases, and credentials are machine-local and never stored here.

use std::fmt;

use qwerio_core::connection::EngineKind;
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::{CatalogPath, ObjectRef};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{WorkspaceError, WorkspaceResult};

pub const MANIFEST_VERSION: u32 = 1;
pub const MANIFEST_DIRECTORY: &str = ".qwerio";
pub const MANIFEST_FILE: &str = "workspace.toml";

macro_rules! manifest_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

manifest_id!(
    /// Identifies a workspace across machines; keys its machine-local state.
    WorkspaceId
);
manifest_id!(DocumentId);
manifest_id!(FolderId);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub id: WorkspaceId,
    pub name: String,
    #[serde(default)]
    pub connections: Vec<ConnectionAlias>,
    #[serde(default)]
    pub folders: Vec<FolderEntry>,
    #[serde(default)]
    pub documents: Vec<DocumentEntry>,
}

impl Manifest {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: MANIFEST_VERSION,
            id: WorkspaceId::new(),
            name: name.into(),
            connections: Vec::new(),
            folders: Vec::new(),
            documents: Vec::new(),
        }
    }

    pub fn connection(&self, id: ConnectionId) -> Option<&ConnectionAlias> {
        self.connections
            .iter()
            .find(|connection| connection.id == id)
    }

    pub fn folder(&self, id: FolderId) -> Option<&FolderEntry> {
        self.folders.iter().find(|folder| folder.id == id)
    }

    pub fn document(&self, id: DocumentId) -> Option<&DocumentEntry> {
        self.documents.iter().find(|document| document.id == id)
    }

    pub fn document_by_path(&self, portable_path: &str) -> Option<&DocumentEntry> {
        self.documents
            .iter()
            .find(|document| document.path == portable_path)
    }

    /// Documents associated with an object, matched by location and name so
    /// that a table replaced by a view keeps its documents.
    pub fn documents_for_object(&self, object: &ObjectRef) -> Vec<&DocumentEntry> {
        self.documents
            .iter()
            .filter(|document| {
                document
                    .associations
                    .iter()
                    .any(|associated| associated.same_location_and_name(object))
            })
            .collect()
    }

    pub fn parse(text: &str) -> WorkspaceResult<Self> {
        let table: toml::Table = toml::from_str(text)
            .map_err(|error| WorkspaceError::InvalidManifest(error.to_string()))?;
        let upgraded = upgrade(table)?;

        upgraded
            .try_into()
            .map_err(|error: toml::de::Error| WorkspaceError::InvalidManifest(error.to_string()))
    }

    pub fn to_toml(&self) -> WorkspaceResult<String> {
        toml::to_string_pretty(self)
            .map_err(|error| WorkspaceError::InvalidManifest(error.to_string()))
    }
}

/// Brings an older manifest to the current version. Manifests written by a
/// newer version are refused rather than rewritten, so they are never
/// downgraded by an older installation.
fn upgrade(table: toml::Table) -> WorkspaceResult<toml::Table> {
    let version = table
        .get("version")
        .and_then(toml::Value::as_integer)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| {
            WorkspaceError::InvalidManifest("missing or invalid \"version\"".to_owned())
        })?;

    match version {
        MANIFEST_VERSION => Ok(table),
        found if found > MANIFEST_VERSION => Err(WorkspaceError::NewerManifest {
            found,
            supported: MANIFEST_VERSION,
        }),
        found => Err(WorkspaceError::InvalidManifest(format!(
            "unknown manifest version {found}"
        ))),
    }
}

/// A logical connection alias. Each machine binds it to an endpoint or file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionAlias {
    pub id: ConnectionId,
    pub name: String,
    pub engine: EngineKind,
}

/// Where an entry appears in the explorer. Placement is organization only:
/// moving an entry never changes a database or moves a file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Placement {
    #[default]
    Root,
    Folder {
        folder: FolderId,
    },
    Container {
        connection: ConnectionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        database: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
}

impl Placement {
    pub fn container(connection: ConnectionId, path: &CatalogPath) -> Self {
        Self::Container {
            connection,
            database: path.database.clone(),
            namespace: path.namespace.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderEntry {
    pub id: FolderId,
    pub name: String,
    #[serde(default)]
    pub placement: Placement,
}

/// Where a SQL document runs, independent of where it is placed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionBinding {
    pub connection: ConnectionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentEntry {
    pub id: DocumentId,
    /// Workspace-relative path with `/` separators.
    pub path: String,
    #[serde(default)]
    pub placement: Placement,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub associations: Vec<ObjectRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionBinding>,
}

impl DocumentEntry {
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use qwerio_core::resource::ObjectKind;

    use super::*;

    fn sample() -> Manifest {
        let connection = ConnectionId::new();
        let mut manifest = Manifest::new("Sales");
        manifest.connections.push(ConnectionAlias {
            id: connection,
            name: "Warehouse".to_owned(),
            engine: EngineKind::Postgres,
        });
        manifest.documents.push(DocumentEntry {
            id: DocumentId::new(),
            path: "queries/top.sql".to_owned(),
            placement: Placement::Container {
                connection,
                database: Some("sales".to_owned()),
                namespace: Some("public".to_owned()),
            },
            associations: vec![ObjectRef {
                connection,
                database: Some("sales".to_owned()),
                namespace: Some("public".to_owned()),
                kind: ObjectKind::Table,
                name: "orders".to_owned(),
            }],
            execution: Some(ExecutionBinding {
                connection,
                database: Some("sales".to_owned()),
            }),
        });
        manifest
    }

    #[test]
    fn manifests_round_trip_through_toml() {
        let manifest = sample();
        let text = manifest.to_toml().expect("serialize");

        assert_eq!(Manifest::parse(&text).expect("parse"), manifest);
    }

    #[test]
    fn manifests_contain_no_machine_local_settings() {
        let text = sample().to_toml().expect("serialize");

        for forbidden in ["host", "port", "password", "user"] {
            assert!(
                !text.contains(&format!("{forbidden} =")),
                "{forbidden} leaked into:\n{text}"
            );
        }
    }

    #[test]
    fn newer_manifests_are_refused() {
        let text = sample()
            .to_toml()
            .expect("serialize")
            .replace("version = 1", "version = 99");

        assert!(matches!(
            Manifest::parse(&text),
            Err(WorkspaceError::NewerManifest { found: 99, .. })
        ));
    }

    #[test]
    fn manifests_without_versions_are_invalid() {
        assert!(matches!(
            Manifest::parse("name = \"x\""),
            Err(WorkspaceError::InvalidManifest(_))
        ));
    }

    #[test]
    fn documents_match_objects_whose_kind_changed() {
        let manifest = sample();
        let mut as_view = manifest.documents[0].associations[0].clone();
        as_view.kind = ObjectKind::View;

        assert_eq!(manifest.documents_for_object(&as_view).len(), 1);
    }
}
