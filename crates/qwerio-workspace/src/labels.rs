//! Label definitions and assignments shared by workspace files and Settings.
use crate::manifest::{DocumentId, FolderId};
use crate::{WorkspaceError, WorkspaceResult};
use qwerio_core::{ids::ConnectionId, resource::ObjectRef};
use serde::{Deserialize, Serialize};

pub fn default_labels() -> Vec<String> {
    ["prod", "test", "staging", "dev"]
        .map(str::to_owned)
        .to_vec()
}

pub fn validate_label(name: &str) -> WorkspaceResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(WorkspaceError::InvalidName(
            "Use a label of 1–64 printable characters".into(),
        ));
    }
    Ok(name.to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LabelTarget {
    Connection { connection: ConnectionId },
    Object { object: ObjectRef },
    Document { document: DocumentId },
    Folder { folder: FolderId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelAssignment {
    pub target: LabelTarget,
    pub labels: Vec<String>,
}

impl LabelTarget {
    pub fn from_location(location: &crate::explorer::Location) -> Option<Self> {
        use crate::explorer::Location;
        match location {
            Location::Container { connection, path }
                if *path == qwerio_core::resource::CatalogPath::root() =>
            {
                Some(Self::Connection {
                    connection: *connection,
                })
            }
            Location::Object(object) => Some(Self::Object {
                object: object.clone(),
            }),
            Location::Document(document) => Some(Self::Document {
                document: *document,
            }),
            Location::Folder(folder) => Some(Self::Folder { folder: *folder }),
            _ => None,
        }
    }
}
