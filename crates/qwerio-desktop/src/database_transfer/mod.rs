//! Database context menus and the backup/restore dialog.

mod jobs;
mod view;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{
    App, AppContext as _, Context, Entity, ParentElement as _, SharedString, Styled as _,
    Subscription, Window, px,
};
use qwerio_app::database_transfer::tools::DetectedTool;
use qwerio_app::database_transfer::{Format, Operation};
use qwerio_core::connection::{ConnectionOptions, EngineKind};
use qwerio_core::ids::ConnectionId;
use qwerio_workspace::explorer::Location;

use crate::theme::glass;
use crate::workspace_model::WorkspaceModel;

const DIALOG_WIDTH: f32 = 600.;

pub fn append_menu(
    menu: PopupMenu,
    model: Entity<WorkspaceModel>,
    location: Location,
    cx: &App,
) -> PopupMenu {
    if target(model.read(cx), &location).is_none() {
        return menu;
    }
    [Operation::Backup, Operation::Restore]
        .into_iter()
        .fold(menu.separator(), |menu, operation| {
            let model = model.clone();
            let location = location.clone();
            menu.item(
                PopupMenuItem::new(format!("{}…", operation.label())).on_click(
                    move |_, window, cx| {
                        open(model.clone(), location.clone(), operation, window, cx);
                    },
                ),
            )
        })
}

fn target(
    model: &WorkspaceModel,
    location: &Location,
) -> Option<(ConnectionId, EngineKind, Option<String>)> {
    let Location::Container { connection, path } = location else {
        return None;
    };
    let alias = model.manifest().connection(*connection)?;
    if path.namespace.is_some() || (alias.engine != EngineKind::Sqlite && path.database.is_none()) {
        return None;
    }
    Some((*connection, alias.engine, path.database.clone()))
}

pub fn available(model: &WorkspaceModel, location: &Location) -> bool {
    target(model, location).is_some()
}

pub fn open(
    model: Entity<WorkspaceModel>,
    location: Location,
    operation: Operation,
    window: &mut Window,
    cx: &mut App,
) {
    let Some((connection, engine, database)) = target(model.read(cx), &location) else {
        return;
    };
    let form =
        cx.new(|cx| TransferForm::new(model, connection, engine, database, operation, window, cx));
    form.update(cx, |form, cx| {
        form.load_connection(window, cx);
        form.check_tools(window, cx);
    });
    glass::open_dialog(window, cx, move |dialog, _, cx| {
        let is_running = form.read(cx).is_running;
        dialog
            .title(operation.label())
            .w(px(DIALOG_WIDTH))
            .overlay_closable(!is_running)
            .keyboard(!is_running)
            .close_button(!is_running)
            .child(form.clone())
    });
}

struct TransferForm {
    model: Entity<WorkspaceModel>,
    connection: ConnectionId,
    engine: EngineKind,
    database: Option<String>,
    operation: Operation,
    format: Format,
    options: Option<ConnectionOptions>,
    file: Entity<InputState>,
    restored_file: Entity<InputState>,
    tool_directory: Entity<InputState>,
    tool: Option<DetectedTool>,
    tool_error: Option<SharedString>,
    error: Option<SharedString>,
    notice: Option<SharedString>,
    is_checking: bool,
    is_running: bool,
    has_confirmed_restore: bool,
    cancelled: Arc<AtomicBool>,
    _subscriptions: Vec<Subscription>,
}

impl Drop for TransferForm {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl TransferForm {
    fn new(
        model: Entity<WorkspaceModel>,
        connection: ConnectionId,
        engine: EngineKind,
        database: Option<String>,
        operation: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let file = cx.new(|cx| InputState::new(window, cx).placeholder("Choose a file"));
        let restored_file =
            cx.new(|cx| InputState::new(window, cx).placeholder("Choose a new database file"));
        let tool_directory = cx.new(|cx| InputState::new(window, cx).placeholder("Auto-detect"));
        let mut subscriptions: Vec<_> = [&file, &restored_file]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.has_confirmed_restore = false;
                        this.notice = None;
                        cx.notify();
                    }
                })
            })
            .collect();
        subscriptions.push(
            cx.subscribe(&tool_directory, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.tool = None;
                    this.tool_error = None;
                    cx.notify();
                }
            }),
        );
        Self {
            model,
            connection,
            engine,
            database,
            operation,
            format: if engine == EngineKind::Postgres {
                Format::Archive
            } else {
                Format::Sql
            },
            options: None,
            file,
            restored_file,
            tool_directory,
            tool: None,
            tool_error: None,
            error: None,
            notice: None,
            is_checking: false,
            is_running: false,
            has_confirmed_restore: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            _subscriptions: subscriptions,
        }
    }

    fn text(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().to_string()
    }

    fn can_start(&self, cx: &App) -> bool {
        !self.is_running
            && !self.is_checking
            && self.options.is_some()
            && (self.engine == EngineKind::Sqlite || self.tool.is_some())
            && !Self::text(&self.file, cx).is_empty()
            && (self.operation != Operation::Restore || self.has_confirmed_restore)
            && (self.engine != EngineKind::Sqlite
                || self.operation != Operation::Restore
                || !Self::text(&self.restored_file, cx).is_empty())
    }

    fn suggested_name(&self) -> String {
        let stem = self.database.as_deref().unwrap_or("database");
        let safe_stem: String = stem
            .chars()
            .map(|character| {
                if character.is_alphanumeric() || matches!(character, '-' | '_') {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        let extension = match (self.engine, self.format) {
            (EngineKind::Sqlite, _) => "sqlite",
            (EngineKind::Postgres, Format::Archive) => "dump",
            _ => "sql",
        };
        format!("{safe_stem}-backup.{extension}")
    }

    fn selected_tool_directory(&self, cx: &App) -> Option<PathBuf> {
        let text = Self::text(&self.tool_directory, cx);
        (!text.is_empty()).then(|| PathBuf::from(text))
    }
}
