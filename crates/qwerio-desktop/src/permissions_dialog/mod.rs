//! Users, roles, and their access to one database object. Changes are staged
//! and shown as SQL before anything runs on the server.

mod create;
mod directory;
mod identity;
mod memberships;
mod pending;
mod scope;
mod sidebar;
mod view;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, ParentElement as _, SharedString, Subscription, Window,
    px,
};
use qwerio_core::connection::{EngineKind, Secret};
use qwerio_core::ids::ConnectionId;
use qwerio_core::permissions::{
    PermissionChange, PermissionScope, PermissionSnapshot, Principal, ScopeKind,
};
use qwerio_workspace::explorer::Location;

use crate::theme::glass;
use crate::workspace_model::WorkspaceModel;
use pending::PendingChange;

const DIALOG_WIDTH: f32 = 960.;
const DEFAULT_USER_HOST: &str = "localhost";
const DEFAULT_ROLE_HOST: &str = "%";
const DEFAULT_SCHEMA: &str = "public";

/// What the main area shows beside the list of users and roles.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pane {
    /// Everyone with direct access to the scope.
    Overview,
    Identity(Principal),
    NewUser,
    NewRole,
}

pub struct PermissionsForm {
    model: Entity<WorkspaceModel>,
    connection: ConnectionId,
    engine: EngineKind,
    pane: Pane,
    /// The object whose permissions are shown, fixed when the dialog opens.
    scope: PermissionScope,
    search: Entity<InputState>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    password: Entity<InputState>,
    shows_built_in: bool,
    shows_sql: bool,
    pending: Vec<PendingChange>,
    snapshot: Option<PermissionSnapshot>,
    loaded_scope: Option<PermissionScope>,
    is_busy: bool,
    error: Option<SharedString>,
    notice: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

pub fn available(model: &WorkspaceModel, location: &Location) -> bool {
    qwerio_app::schema::location_connection(location)
        .and_then(|connection| model.manifest().connection(connection))
        .is_some_and(|alias| alias.engine != EngineKind::Sqlite)
}

pub fn open(model: Entity<WorkspaceModel>, location: Location, window: &mut Window, cx: &mut App) {
    let Some(connection) = qwerio_app::schema::location_connection(&location) else {
        return;
    };
    if !available(model.read(cx), &location) {
        return;
    }
    let form = cx.new(|cx| PermissionsForm::new(model, connection, location, window, cx));
    form.update(cx, |form, cx| form.load(window, cx));
    glass::open_dialog(window, cx, move |dialog, _, _| {
        dialog
            .title("Users & permissions")
            .w(px(DIALOG_WIDTH))
            .child(form.clone())
    });
}

impl PermissionsForm {
    fn new(
        model: Entity<WorkspaceModel>,
        connection: ConnectionId,
        location: Location,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let engine = model
            .read(cx)
            .manifest()
            .connection(connection)
            .map(|alias| alias.engine)
            .unwrap_or(EngineKind::Postgres);
        let (database, namespace, object) = match &location {
            Location::Container { path, .. } => {
                (path.database.clone(), path.namespace.clone(), None)
            }
            Location::Object(object) => (
                object.database.clone(),
                object.namespace.clone(),
                Some(object.clone()),
            ),
            _ => (None, None, None),
        };
        let kind = match &object {
            Some(_) => ScopeKind::Table,
            None if namespace.is_some() => ScopeKind::Schema,
            None if engine == EngineKind::Postgres || database.is_some() => ScopeKind::Database,
            None => ScopeKind::Server,
        };

        let mut input = |placeholder: &'static str, value: String, masked| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
                    .masked(masked)
            })
        };
        let search = input("Search users and roles", String::new(), false);
        let name = input("Name", String::new(), false);
        let host = input("Host", DEFAULT_USER_HOST.to_owned(), false);
        let password = input("Password", String::new(), true);
        let scope = PermissionScope {
            kind,
            database: database.unwrap_or_default(),
            namespace: namespace.unwrap_or_else(|| DEFAULT_SCHEMA.to_owned()),
            object: object.map(|object| object.name).unwrap_or_default(),
            column: String::new(),
        };

        let inputs = [&search, &name, &host, &password];
        let subscriptions = inputs
            .iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.notice = None;
                        cx.notify();
                    }
                })
            })
            .collect();

        Self {
            model,
            connection,
            engine,
            pane: Pane::Overview,
            scope,
            search,
            name,
            host,
            password,
            shows_built_in: false,
            shows_sql: false,
            pending: Vec::new(),
            snapshot: None,
            loaded_scope: None,
            is_busy: false,
            error: None,
            notice: None,
            _subscriptions: subscriptions,
        }
    }

    fn text(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().to_string()
    }

    /// The scope, once its grants have been read.
    fn shown_scope(&self) -> Option<PermissionScope> {
        self.loaded_scope.clone()
    }

    fn supports_roles(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.supports_roles)
    }

    fn literal_database_names(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.literal_database_names)
    }

    fn preview(&self, change: &PermissionChange) -> Result<String, String> {
        change
            .preview_for_server(self.engine, self.literal_database_names())
            .map_err(|error| error.message)
    }

    fn stage(&mut self, change: PendingChange, cx: &mut Context<Self>) {
        self.pending = pending::toggled(&self.pending, change);
        self.notice = None;
        cx.notify();
    }

    fn show(&mut self, pane: Pane, cx: &mut Context<Self>) {
        self.pane = pane;
        self.error = None;
        self.notice = None;
        cx.notify();
    }

    fn open_create(&mut self, is_role: bool, window: &mut Window, cx: &mut Context<Self>) {
        let host = if is_role {
            DEFAULT_ROLE_HOST
        } else {
            DEFAULT_USER_HOST
        };
        self.name
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.password
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.host
            .update(cx, |input, cx| input.set_value(host, window, cx));
        let pane = if is_role {
            Pane::NewRole
        } else {
            Pane::NewUser
        };
        self.show(pane, cx);
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_busy {
            return;
        }
        let scope = self.scope.clone();
        let scope = qwerio_core::permissions::grants_query(self.engine, &scope)
            .is_ok()
            .then_some(scope);
        let load = self
            .model
            .read(cx)
            .inspect_permissions(self.connection, scope.clone(), cx);
        self.is_busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            this.update_in(cx, |this, window, cx| {
                this.is_busy = false;
                match result {
                    Ok(snapshot) => {
                        // Opened from a server: the grants follow once the
                        // session reports its default database.
                        let default_database = snapshot
                            .database
                            .clone()
                            .filter(|_| this.scope.database.is_empty());
                        this.snapshot = Some(snapshot);
                        this.loaded_scope = scope;
                        if let Some(database) = default_database {
                            this.scope.database = database;
                            this.load(window, cx);
                        }
                    }
                    Err(error) => {
                        this.snapshot = None;
                        this.loaded_scope = None;
                        this.error = Some(error);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Creates the user or role right away, then opens it.
    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((change, principal)) = self.create_change(cx) else {
            return;
        };
        if self.is_busy || self.preview(&change).is_err() {
            return;
        }
        let apply = self
            .model
            .read(cx)
            .apply_permission_change(self.connection, change, cx);
        self.is_busy = true;
        self.error = None;
        self.notice = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = apply.await;
            this.update_in(cx, |this, window, cx| {
                this.is_busy = false;
                match result {
                    Ok(()) => {
                        this.password
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        this.notice = Some(format!("Created {}", principal.label()).into());
                        this.pane = Pane::Identity(principal);
                        this.load(window, cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Runs the staged changes in order and stops at the first failure,
    /// keeping it and the changes after it staged.
    fn apply_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_busy || self.pending.is_empty() {
            return;
        }
        let changes = self.pending.clone();
        self.is_busy = true;
        self.error = None;
        self.notice = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let mut applied = 0;
            let mut failure = None;
            for change in &changes {
                let Ok(apply) = this.update(cx, |this, cx| {
                    this.model.read(cx).apply_permission_change(
                        this.connection,
                        change.to_change(),
                        cx,
                    )
                }) else {
                    return;
                };
                match apply.await {
                    Ok(()) => applied += 1,
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            this.update_in(cx, |this, window, cx| {
                this.is_busy = false;
                this.pending = changes[applied..].to_vec();
                match failure {
                    None => this.notice = Some(applied_notice(applied).into()),
                    Some(error) if applied == 0 => this.error = Some(error),
                    Some(error) => {
                        this.error = Some(
                            format!(
                                "{} The changes before it were applied. {error}",
                                failed_change_notice(applied, changes.len())
                            )
                            .into(),
                        )
                    }
                }
                this.load(window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn create_change(&self, cx: &App) -> Option<(PermissionChange, Principal)> {
        let is_role = match self.pane {
            Pane::NewUser => false,
            Pane::NewRole => true,
            _ => return None,
        };
        let has_host = match self.engine {
            EngineKind::Mysql => true,
            EngineKind::Mariadb => !is_role,
            _ => false,
        };
        let principal = Principal {
            name: Self::text(&self.name, cx),
            host: has_host.then(|| Self::text(&self.host, cx)),
        };
        let change = if is_role {
            PermissionChange::CreateRole {
                role: principal.clone(),
            }
        } else {
            PermissionChange::CreateUser {
                principal: principal.clone(),
                password: Secret::new(Self::text(&self.password, cx)),
            }
        };
        Some((change, principal))
    }
}

fn applied_notice(count: usize) -> String {
    match count {
        1 => "Applied 1 change".to_owned(),
        count => format!("Applied {count} changes"),
    }
}

fn failed_change_notice(applied: usize, total: usize) -> String {
    format!("Change {} of {total} failed.", applied + 1)
}
