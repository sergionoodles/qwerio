//! Shared vocabulary between tab content views and the shell: requests a view
//! cannot fulfill itself, and icons for explorer entries.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::{Icon, IconName};
use qwerio_app::navigation::ViewState;
use qwerio_core::connection::EngineKind;
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::{ObjectKind, ObjectRef};
use qwerio_workspace::MovableEntry;
use qwerio_workspace::document::DocumentKind;
use qwerio_workspace::explorer::{EntryKind, Location, ScratchId};
use qwerio_workspace::manifest::{DocumentId, Placement};

/// Requests from content views, handled by the workspace shell.
#[derive(Clone, Debug)]
pub enum ContentEvent {
    Navigate {
        location: Location,
        view: ViewState,
        new_tab: bool,
    },
    /// Register an untracked file and open it.
    OpenUntracked(String),
    /// Set up (or edit) the machine-local endpoint of a connection.
    SetUpConnection(ConnectionId),
    /// Create a document beside the current container, optionally linked to
    /// an object.
    NewDocument {
        kind: DocumentKind,
        object: Option<ObjectRef>,
    },
    NewFolder,
    RenameDocument(DocumentId),
    RemoveDocument {
        document: DocumentId,
        delete_file: bool,
    },
    AddFavorite(Location),
    /// Change where an entry appears; files and database objects stay put.
    MoveEntry {
        entry: MovableEntry,
        placement: Placement,
    },
    Associate {
        document: DocumentId,
        object: ObjectRef,
    },
    Dissociate {
        document: DocumentId,
        object: ObjectRef,
    },
    /// Save a temporary query as a workspace document.
    SaveScratch {
        scratch: ScratchId,
        text: String,
    },
    /// Title, status, or modification state changed.
    Changed,
}

pub fn entry_icon(kind: EntryKind) -> Icon {
    match kind {
        EntryKind::Folder => Icon::new(IconName::Folder),
        EntryKind::Connection(EngineKind::Postgres) => Icon::new(LucideIcon::Server),
        EntryKind::Connection(EngineKind::Sqlite) => Icon::new(LucideIcon::Database),
        EntryKind::Database => Icon::new(LucideIcon::Database),
        EntryKind::Namespace { .. } => Icon::new(LucideIcon::LayoutGrid),
        EntryKind::Object(kind) => object_icon(kind),
        EntryKind::Document(kind) => document_icon(kind),
    }
}

pub fn object_icon(kind: ObjectKind) -> Icon {
    match kind {
        ObjectKind::Table | ObjectKind::ForeignTable => Icon::new(LucideIcon::Table),
        ObjectKind::View | ObjectKind::MaterializedView => Icon::new(LucideIcon::Sheet),
    }
}

pub fn document_icon(kind: DocumentKind) -> Icon {
    match kind {
        DocumentKind::Sql => Icon::new(LucideIcon::FileCode),
        DocumentKind::Markdown => Icon::new(LucideIcon::NotebookPen),
        DocumentKind::Text => Icon::new(IconName::FileText),
    }
}
