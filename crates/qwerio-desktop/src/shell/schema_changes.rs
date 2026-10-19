//! New databases, tables, and columns. Each command applies where the
//! focused tab is; after a change, the new database or table opens.

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, Context, Window};
use qwerio_app::navigation::ViewState;
use qwerio_app::schema::{SchemaTargets, location_connection, schema_targets};
use qwerio_core::ddl::SchemaChange;
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::{CatalogPath, ObjectKind, ObjectRef};
use qwerio_workspace::explorer::Location;

use super::WorkspaceShell;
use crate::connection_dialog;
use crate::schema_dialog::{self, SchemaTarget};

const NEEDS_CONNECTION: &str = "Open a connection to create a database in it";
const NEEDS_CONTAINER: &str = "Open a schema or one of its tables to create a table there";
const NEEDS_TABLE: &str = "Open a table to add a column to it";

impl WorkspaceShell {
    /// What can be created from the focused location.
    pub(super) fn schema_targets(&self, cx: &App) -> SchemaTargets {
        let location = self.focused_location();
        let model = self.model.read(cx);
        let capabilities =
            location_connection(&location).and_then(|connection| model.capabilities(connection));

        schema_targets(&location, capabilities)
    }

    /// A server database from a form, or a new file for single-file engines.
    pub(super) fn new_database(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = self.schema_targets(cx).database else {
            return window.push_notification(Notification::info(NEEDS_CONNECTION), cx);
        };

        let has_databases = self
            .model
            .read(cx)
            .capabilities(connection)
            .is_some_and(|capabilities| capabilities.has_databases);

        if !has_databases {
            return self.new_database_file(window, cx);
        }

        self.open_schema_dialog(SchemaTarget::Database { connection }, window, cx);
    }

    pub(super) fn new_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((connection, container)) = self.schema_targets(cx).table else {
            return window.push_notification(Notification::info(NEEDS_CONTAINER), cx);
        };

        self.open_schema_dialog(
            SchemaTarget::Table {
                connection,
                container,
            },
            window,
            cx,
        );
    }

    pub(super) fn new_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(table) = self.schema_targets(cx).column else {
            return window.push_notification(Notification::info(NEEDS_TABLE), cx);
        };

        self.open_schema_dialog(SchemaTarget::Column { table }, window, cx);
    }

    fn open_schema_dialog(
        &mut self,
        target: SchemaTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection = target.connection();
        let Some(capabilities) = self.model.read(cx).capabilities(connection).cloned() else {
            return;
        };

        let this = cx.entity().downgrade();
        let on_created: schema_dialog::OnCreated = Rc::new(move |change, window, cx| {
            this.update(cx, |this, cx| {
                this.after_schema_change(connection, change, window, cx)
            })
            .ok();
        });

        schema_dialog::open(
            self.model.clone(),
            target,
            &capabilities,
            on_created,
            window,
            cx,
        );
    }

    fn after_schema_change(
        &mut self,
        connection: ConnectionId,
        change: &SchemaChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let location = match change {
            SchemaChange::CreateDatabase { name } => Location::Container {
                connection,
                path: CatalogPath::database(name.clone()),
            },
            SchemaChange::CreateTable {
                container, name, ..
            } => Location::Object(ObjectRef {
                connection,
                database: container.database.clone(),
                namespace: container.namespace.clone(),
                kind: ObjectKind::Table,
                name: name.clone(),
            }),
            SchemaChange::AddColumn { table, .. } => {
                if self.focused_location() == Location::Object(table.clone()) {
                    self.refresh(window, cx);
                }

                return;
            }
        };

        self.navigate(location, ViewState::default(), false, window, cx);
    }

    fn new_database_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let create = connection_dialog::create_sqlite_database(&self.model, cx);

        cx.spawn_in(window, async move |this, cx| {
            let result = create.await;

            this.update_in(cx, |this, window, cx| {
                if let Some(Some(connection)) = Self::report(result, window, cx) {
                    this.navigate(
                        Location::connection_root(connection),
                        ViewState::default(),
                        false,
                        window,
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }
}
