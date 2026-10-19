//! Creating a database, table, or column from a small form. The statement
//! the form describes is shown while typing and runs only on Create.

mod column_row;
mod view;

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogClose;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{Disableable as _, WindowExt as _, h_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, ParentElement as _, SharedString,
    Styled as _, Subscription, Window, px,
};
use qwerio_core::capabilities::DatabaseCapabilities;
use qwerio_core::ddl::SchemaChange;
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::{CatalogPath, ObjectRef};

use self::column_row::ColumnRow;
use crate::workspace_model::WorkspaceModel;

const TABLE_DIALOG_WIDTH: f32 = 680.;
const DIALOG_WIDTH: f32 = 480.;
const KEY_COLUMN_NAME: &str = "id";

/// What the form creates, and where.
#[derive(Clone, Debug)]
pub enum SchemaTarget {
    Database {
        connection: ConnectionId,
    },
    Table {
        connection: ConnectionId,
        container: CatalogPath,
    },
    Column {
        table: ObjectRef,
    },
}

impl SchemaTarget {
    pub fn connection(&self) -> ConnectionId {
        match self {
            Self::Database { connection } | Self::Table { connection, .. } => *connection,
            Self::Column { table } => table.connection,
        }
    }

    fn title(&self) -> SharedString {
        match self {
            Self::Database { .. } => "New Database".into(),
            Self::Table { container, .. } => match &container.namespace {
                Some(namespace) => format!("New Table in {namespace}").into(),
                None => "New Table".into(),
            },
            Self::Column { table } => format!("New Column in {}", table.qualified_name()).into(),
        }
    }
}

/// Called after the change was applied and the dialog closed.
pub type OnCreated = Rc<dyn Fn(&SchemaChange, &mut Window, &mut App)>;

enum Fields {
    Database {
        name: Entity<InputState>,
    },
    Table {
        container: CatalogPath,
        name: Entity<InputState>,
        columns: Vec<ColumnRow>,
    },
    Column {
        table: ObjectRef,
        column: ColumnRow,
        default: Entity<InputState>,
    },
}

pub struct SchemaForm {
    model: Entity<WorkspaceModel>,
    connection: ConnectionId,
    fields: Fields,
    column_types: Vec<SharedString>,
    next_row_id: usize,
    on_created: OnCreated,
    error: Option<SharedString>,
    is_busy: bool,
    _subscriptions: Vec<Subscription>,
}

pub fn open(
    model: Entity<WorkspaceModel>,
    target: SchemaTarget,
    capabilities: &DatabaseCapabilities,
    on_created: OnCreated,
    window: &mut Window,
    cx: &mut App,
) {
    let title = target.title();
    let width = match target {
        SchemaTarget::Table { .. } => TABLE_DIALOG_WIDTH,
        _ => DIALOG_WIDTH,
    };
    let form = cx.new(|cx| SchemaForm::new(model, target, capabilities, on_created, window, cx));
    let focus = Cell::new(true);

    window.open_dialog(cx, move |dialog, window, cx| {
        if focus.replace(false) {
            let handle = form.read(cx).first_input().read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }

        let can_create = form.read(cx).can_create(cx);
        let ok_form = form.clone();
        let create_form = form.clone();

        dialog
            .title(title.clone())
            .w(px(width))
            .child(form.clone())
            .on_ok(move |_, window, cx| {
                ok_form.update(cx, |form, cx| form.create(window, cx));
                false
            })
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        DialogClose::new()
                            .child(Button::new("schema-cancel").ghost().label("Cancel")),
                    )
                    .child(
                        Button::new("schema-create")
                            .primary()
                            .label("Create")
                            .disabled(!can_create)
                            .on_click(move |_, window, cx| {
                                create_form.update(cx, |form, cx| form.create(window, cx))
                            }),
                    ),
            )
    });
}

impl SchemaForm {
    fn new(
        model: Entity<WorkspaceModel>,
        target: SchemaTarget,
        capabilities: &DatabaseCapabilities,
        on_created: OnCreated,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let first_type = capabilities
            .column_types
            .first()
            .cloned()
            .unwrap_or_default();
        let name_input =
            |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
                cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
            };

        let connection = target.connection();
        let fields = match target {
            SchemaTarget::Database { .. } => Fields::Database {
                name: name_input("Database name", window, cx),
            },
            SchemaTarget::Table { container, .. } => Fields::Table {
                container,
                name: name_input("Table name", window, cx),
                columns: vec![ColumnRow::new(
                    0,
                    KEY_COLUMN_NAME,
                    &capabilities.key_column_type,
                    true,
                    window,
                    cx,
                )],
            },
            SchemaTarget::Column { table } => Fields::Column {
                table,
                column: ColumnRow::new(0, "", &first_type, false, window, cx),
                default: name_input("None", window, cx),
            },
        };

        let mut form = Self {
            model,
            connection,
            fields,
            column_types: capabilities
                .column_types
                .iter()
                .map(|name| SharedString::from(name.clone()))
                .collect(),
            next_row_id: 1,
            on_created,
            error: None,
            is_busy: false,
            _subscriptions: Vec::new(),
        };

        let inputs = form.inputs();
        form._subscriptions = inputs
            .iter()
            .map(|input| Self::observe_input(input, cx))
            .collect();
        form
    }

    /// Re-renders the preview as the user types.
    fn observe_input(input: &Entity<InputState>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.error = None;
                cx.notify();
            }
        })
    }

    fn inputs(&self) -> Vec<Entity<InputState>> {
        match &self.fields {
            Fields::Database { name } => vec![name.clone()],
            Fields::Table { name, columns, .. } => std::iter::once(name.clone())
                .chain(
                    columns
                        .iter()
                        .flat_map(|row| [row.name.clone(), row.data_type.clone()]),
                )
                .collect(),
            Fields::Column {
                column, default, ..
            } => vec![
                column.name.clone(),
                column.data_type.clone(),
                default.clone(),
            ],
        }
    }

    fn first_input(&self) -> &Entity<InputState> {
        match &self.fields {
            Fields::Database { name } | Fields::Table { name, .. } => name,
            Fields::Column { column, .. } => &column.name,
        }
    }

    fn change(&self, cx: &App) -> SchemaChange {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();

        match &self.fields {
            Fields::Database { name } => SchemaChange::CreateDatabase { name: text(name) },
            Fields::Table {
                container,
                name,
                columns,
            } => SchemaChange::CreateTable {
                container: container.clone(),
                name: text(name),
                columns: columns.iter().map(|row| row.definition(None, cx)).collect(),
            },
            Fields::Column {
                table,
                column,
                default,
            } => {
                let default = Some(text(default)).filter(|default| !default.is_empty());
                SchemaChange::AddColumn {
                    table: table.clone(),
                    column: column.definition(default, cx),
                }
            }
        }
    }

    fn can_create(&self, cx: &App) -> bool {
        !self.is_busy && self.change(cx).render().is_ok()
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_create(cx) {
            return;
        }

        let change = self.change(cx);
        let connection = self.connection;
        let apply = self.model.update(cx, |model, cx| {
            model.apply_schema_change(connection, change.clone(), cx)
        });
        let on_created = Rc::clone(&self.on_created);

        self.is_busy = true;
        self.error = None;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = apply.await;

            match result {
                Ok(()) => cx
                    .update(|window, cx| {
                        window.close_dialog(cx);
                        on_created(&change, window, cx);
                    })
                    .ok(),
                Err(error) => this
                    .update(cx, |this, cx| {
                        this.error = Some(error);
                        this.is_busy = false;
                        cx.notify();
                    })
                    .ok(),
            };
        })
        .detach();
    }

    fn add_column_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(data_type) = self.column_types.first().cloned() else {
            return;
        };

        let row = ColumnRow::new(self.next_row_id, "", &data_type, false, window, cx);
        self.next_row_id += 1;
        self._subscriptions
            .extend([&row.name, &row.data_type].map(|input| Self::observe_input(input, cx)));

        let handle = row.name.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        if let Fields::Table { columns, .. } = &mut self.fields {
            columns.push(row);
        }

        cx.notify();
    }

    fn update_row(
        &mut self,
        id: usize,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut ColumnRow),
    ) {
        let row = match &mut self.fields {
            Fields::Table { columns, .. } => columns.iter_mut().find(|row| row.id == id),
            Fields::Column { column, .. } => Some(column).filter(|column| column.id == id),
            Fields::Database { .. } => None,
        };

        if let Some(row) = row {
            update(row);
            self.error = None;
            cx.notify();
        }
    }

    fn remove_column_row(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Fields::Table { columns, .. } = &mut self.fields {
            columns.retain(|row| row.id != id);
            cx.notify();
        }
    }
}
