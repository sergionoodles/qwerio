//! Shared vocabulary between tab content views and the shell: requests a view
//! cannot fulfill itself, and icons for explorer entries.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::{App, Styled as _};
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
    OpenPermissions(Location),
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

pub fn entry_icon(kind: EntryKind, cx: &App) -> Icon {
    let theme = cx.theme();
    let (icon, color) = match kind {
        EntryKind::Folder => (Icon::new(IconName::Folder), theme.yellow),
        EntryKind::Connection(EngineKind::Postgres | EngineKind::Mysql | EngineKind::Mariadb) => {
            (Icon::new(LucideIcon::Server), theme.blue)
        }
        EntryKind::Connection(EngineKind::Sqlite) | EntryKind::Database => {
            (Icon::new(LucideIcon::Database), theme.blue)
        }
        EntryKind::Namespace { .. } => (Icon::new(LucideIcon::LayoutGrid), theme.magenta),
        EntryKind::Object(kind) => return object_icon(kind, cx),
        EntryKind::Document(kind) => return document_icon(kind, cx),
    };

    icon.text_color(color)
}

pub fn object_icon(kind: ObjectKind, cx: &App) -> Icon {
    let (icon, color) = match kind {
        ObjectKind::Table | ObjectKind::ForeignTable => (LucideIcon::Table, cx.theme().green),
        ObjectKind::View | ObjectKind::MaterializedView => (LucideIcon::Sheet, cx.theme().cyan),
    };

    Icon::new(icon).text_color(color)
}

pub fn document_icon(kind: DocumentKind, cx: &App) -> Icon {
    let (icon, color) = match kind {
        DocumentKind::Sql => (Icon::new(LucideIcon::FileCode), cx.theme().magenta),
        DocumentKind::Markdown => (Icon::new(LucideIcon::NotebookPen), cx.theme().yellow),
        DocumentKind::Text => (Icon::new(IconName::FileText), cx.theme().muted_foreground),
    };

    icon.text_color(color)
}
