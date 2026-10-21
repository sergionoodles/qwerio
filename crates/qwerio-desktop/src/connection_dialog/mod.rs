//! Creating or editing a connection. File workspaces save connection details
//! and passwords inside the workspace's optional encryption. Legacy folders
//! retain their machine-local bindings and platform keyring credentials.
//!
//! A new connection is a two-step wizard: [`engine_picker`] chooses the
//! database, then [`details`] collects its settings. Editing opens straight
//! on the details, since an alias never changes engine.

mod details;
mod engine_picker;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogClose;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, IndexPath, StyledExt as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _,
    PathPromptOptions, Render, SharedString, Styled as _, Subscription, Task, Window, div, px,
};
use qwerio_core::connection::{
    ConnectionDescriptor, ConnectionOptions, Credentials, DEFAULT_MYSQL_PORT,
    DEFAULT_POSTGRES_PORT, EngineKind, MysqlOptions, PostgresOptions, Secret, SqliteOptions,
    TlsMode,
};
use qwerio_core::connection_string::{parse_connection_string, parse_mysql_connection_string};
use qwerio_core::ids::ConnectionId;

use crate::theme::glass;
use crate::workspace_model::{WorkspaceModel, error_text};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DIALOG_WIDTH: f32 = 560.;
/// Every engine the wizard offers, in the order its grid shows them.
const ENGINES: [EngineKind; 4] = [
    EngineKind::Postgres,
    EngineKind::Sqlite,
    EngineKind::Mysql,
    EngineKind::Mariadb,
];
const TLS_LABELS: [&str; 3] = ["Verify certificate", "Verify with CA file", "No encryption"];
const NAME_PLACEHOLDER: &str = "Name shown in the workspace";
const PASSWORD_PLACEHOLDER: &str = "Password";
const STORED_PASSWORD_PLACEHOLDER: &str = "Leave empty to keep the saved password";
const CONNECTION_STRING_PLACEHOLDER: &str = "postgres://user:password@host:5432/database";
const MISSING_CONNECTION_STRING: &str =
    "Paste a connection string, or turn it off to enter the details";
const NEW_SQLITE_FILE_NAME: &str = "database.sqlite";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tls {
    Verify,
    VerifyWithCa,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    ChooseEngine,
    Details,
}

/// What the connection string field last produced.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ConnectionStringState {
    Empty,
    /// The details fields hold the parsed values.
    Applied,
    Invalid(SharedString),
}

pub struct ConnectionForm {
    model: Entity<WorkspaceModel>,
    editing: Option<ConnectionId>,
    step: Step,
    engine: EngineKind,
    /// Server connections: the connection string replaces the endpoint fields.
    use_connection_string: bool,
    connection_string: Entity<InputState>,
    connection_string_state: ConnectionStringState,
    tls_select: Entity<SelectState<Vec<SharedString>>>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    database: Entity<InputState>,
    password: Entity<InputState>,
    ca_file: Entity<InputState>,
    sqlite_path: Entity<InputState>,
    enforce_foreign_keys: bool,
    has_stored_password: bool,
    status: Option<(SharedString, bool)>,
    is_busy: bool,
    _subscriptions: Vec<Subscription>,
}

/// Opens the dialog, for a new connection or to edit `editing`.
pub fn open(
    model: Entity<WorkspaceModel>,
    editing: Option<ConnectionId>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| ConnectionForm::new(model, editing, window, cx));
    let title = if editing.is_some() {
        "Connection Settings"
    } else {
        "New Connection"
    };

    glass::open_dialog(window, cx, move |dialog, _, cx| {
        dialog
            .title(title)
            .w(px(DIALOG_WIDTH))
            .child(form.clone())
            .footer(render_footer(&form, cx))
    });
}

fn render_footer(form: &Entity<ConnectionForm>, cx: &App) -> AnyElement {
    let state = form.read(cx);
    let is_busy = state.is_busy;
    let is_new = state.editing.is_none();

    let footer = h_flex().w_full().gap_2();

    if state.step == Step::ChooseEngine {
        let continue_form = form.clone();
        return footer
            .child(cancel_button())
            .child(div().flex_1())
            .child(
                Button::new("connection-continue")
                    .primary()
                    .label("Continue")
                    .on_click(move |_, window, cx| {
                        continue_form.update(cx, |form, cx| form.show_details(window, cx))
                    }),
            )
            .into_any_element();
    }

    let back_form = form.clone();
    let test_form = form.clone();
    let save_form = form.clone();
    let leading = if is_new {
        Button::new("connection-back")
            .ghost()
            .icon(IconName::ArrowLeft)
            .label("Back")
            .disabled(is_busy)
            .on_click(move |_, _, cx| back_form.update(cx, |form, cx| form.show_engines(cx)))
            .into_any_element()
    } else {
        cancel_button().into_any_element()
    };

    footer
        .child(div().flex_none().child(leading))
        .child(div().flex_1())
        .child(
            Button::new("test-connection")
                .outline()
                .label("Test")
                .disabled(is_busy)
                .on_click(move |_, window, cx| {
                    test_form.update(cx, |form, cx| form.test(window, cx))
                }),
        )
        .child(
            Button::new("save-connection")
                .primary()
                .label("Save")
                .disabled(is_busy)
                .on_click(move |_, window, cx| {
                    save_form.update(cx, |form, cx| form.save(window, cx))
                }),
        )
        .into_any_element()
}

fn cancel_button() -> impl IntoElement {
    div()
        .flex_none()
        .child(DialogClose::new().child(Button::new("cancel-connection").ghost().label("Cancel")))
}

/// Asks where to save a new SQLite database, creates the empty file, and
/// adds it to the workspace as a connection. `None` when the user cancels.
pub fn create_sqlite_database(
    model: &Entity<WorkspaceModel>,
    cx: &mut App,
) -> Task<Result<Option<ConnectionId>, SharedString>> {
    let directory = model.read(cx).directory();
    let prompt = cx.prompt_for_new_path(&directory, Some(NEW_SQLITE_FILE_NAME));
    let model = model.clone();

    cx.spawn(async move |cx| {
        // The prompt's sender is dropped when the dialog is dismissed.
        let Ok(chosen) = prompt.await else {
            return Ok(None);
        };
        let Some(path) = chosen.map_err(|error| SharedString::from(error.to_string()))? else {
            return Ok(None);
        };

        let create_path = path.clone();
        cx.background_spawn(async move { qwerio_sqlite::create_database_file(&create_path) })
            .await
            .map_err(|error| SharedString::from(error.message))?;

        let name = default_name(EngineKind::Sqlite, &path.display().to_string())
            .unwrap_or_else(|| NEW_SQLITE_FILE_NAME.to_owned());
        let alias = model.update(cx, |model, cx| {
            model.update_workspace(cx, move |workspace| {
                workspace.add_connection(&name, EngineKind::Sqlite)
            })
        });
        let connection = alias.await?;

        let options = ConnectionOptions::Sqlite(SqliteOptions::new(path));
        let bind = model.update(cx, |model, cx| {
            model.bind_connection(connection, options, None, cx)
        });
        bind.await?;

        model.update(cx, |model, cx| model.set_bound(connection, true, cx));
        Ok(Some(connection))
    })
}

impl ConnectionForm {
    fn new(
        model: Entity<WorkspaceModel>,
        editing: Option<ConnectionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tls_labels: Vec<SharedString> = TLS_LABELS
            .iter()
            .map(|label| SharedString::from(*label))
            .collect();
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };

        let tls_select =
            cx.new(|cx| SelectState::new(tls_labels, Some(IndexPath::new(0)), window, cx));

        // Masked because pasted URLs usually carry the password.
        let connection_string = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(CONNECTION_STRING_PLACEHOLDER)
                .masked(true)
        });
        let host = input("Host or socket directory", window, cx);
        let sqlite_path = input("Database file", window, cx);

        let subscriptions = vec![
            cx.subscribe(
                &tls_select,
                |_, _, _: &SelectEvent<Vec<SharedString>>, cx| cx.notify(),
            ),
            cx.subscribe_in(&connection_string, window, Self::on_connection_string_event),
            cx.subscribe_in(&host, window, Self::on_address_event),
            cx.subscribe_in(&sqlite_path, window, Self::on_address_event),
        ];

        let port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Port")
                .default_value(DEFAULT_POSTGRES_PORT.to_string())
        });
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(PASSWORD_PLACEHOLDER)
                .masked(true)
        });
        let step = if editing.is_some() {
            Step::Details
        } else {
            Step::ChooseEngine
        };

        let mut form = Self {
            name: input(NAME_PLACEHOLDER, window, cx),
            host,
            port,
            user: input("User", window, cx),
            database: input("Database", window, cx),
            password,
            ca_file: input("CA certificate file (PEM)", window, cx),
            sqlite_path,
            model,
            editing,
            step,
            engine: ENGINES[0],
            use_connection_string: false,
            connection_string,
            connection_string_state: ConnectionStringState::Empty,
            tls_select,
            enforce_foreign_keys: true,
            has_stored_password: false,
            status: None,
            is_busy: false,
            _subscriptions: subscriptions,
        };

        if let Some(connection) = editing {
            form.load_existing(connection, window, cx);
        }

        form
    }

    fn choose_engine(&mut self, engine: EngineKind, window: &mut Window, cx: &mut Context<Self>) {
        let old_default = if matches!(self.engine, EngineKind::Mysql | EngineKind::Mariadb) {
            DEFAULT_MYSQL_PORT
        } else {
            DEFAULT_POSTGRES_PORT
        };
        let new_default = if matches!(engine, EngineKind::Mysql | EngineKind::Mariadb) {
            DEFAULT_MYSQL_PORT
        } else {
            DEFAULT_POSTGRES_PORT
        };
        if Self::text(&self.port, cx) == old_default.to_string() {
            self.port.update(cx, |input, cx| {
                input.set_value(new_default.to_string(), window, cx)
            });
        }
        if self.engine != engine {
            self.use_connection_string = false;
            self.connection_string_state = ConnectionStringState::Empty;
            self.connection_string.update(cx, |input, cx| {
                input.set_value("", window, cx);
                let placeholder = if matches!(engine, EngineKind::Mysql | EngineKind::Mariadb) {
                    "mysql://user:password@host:3306/database"
                } else {
                    CONNECTION_STRING_PLACEHOLDER
                };
                input.set_placeholder(placeholder, window, cx);
            });
        }
        self.engine = engine;
        let host_placeholder = if matches!(engine, EngineKind::Mysql | EngineKind::Mariadb) {
            "Host or socket file"
        } else {
            "Host or socket directory"
        };
        self.host.update(cx, |input, cx| {
            input.set_placeholder(host_placeholder, window, cx)
        });
        self.refresh_name_placeholder(window, cx);
        cx.notify();
    }

    fn show_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step = Step::Details;
        self.status = None;
        self.name.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn show_engines(&mut self, cx: &mut Context<Self>) {
        self.step = Step::ChooseEngine;
        self.status = None;
        cx.notify();
    }

    fn set_use_connection_string(
        &mut self,
        use_connection_string: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.use_connection_string = use_connection_string;
        self.status = None;

        if use_connection_string {
            self.connection_string
                .update(cx, |input, cx| input.focus(window, cx));
        }

        cx.notify();
    }

    fn on_address_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.refresh_name_placeholder(window, cx);
        }
    }

    fn on_connection_string_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }

        let text = Self::text(&self.connection_string, cx);
        self.connection_string_state = if text.is_empty() {
            ConnectionStringState::Empty
        } else {
            let parsed = if matches!(self.engine, EngineKind::Mysql | EngineKind::Mariadb) {
                parse_mysql_connection_string(&text).map(|parsed| {
                    let options = if self.engine == EngineKind::Mariadb {
                        ConnectionOptions::Mariadb(parsed.options)
                    } else {
                        ConnectionOptions::Mysql(parsed.options)
                    };
                    (options, parsed.password)
                })
            } else {
                parse_connection_string(&text)
                    .map(|parsed| (ConnectionOptions::Postgres(parsed.options), parsed.password))
            };
            match parsed {
                Ok((options, password)) => {
                    self.apply_connection_string(options, password, window, cx);
                    ConnectionStringState::Applied
                }
                Err(error) => ConnectionStringState::Invalid(error.to_string().into()),
            }
        };
        cx.notify();
    }

    fn apply_connection_string(
        &mut self,
        options: ConnectionOptions,
        password: Option<Secret>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let password = password
            .as_ref()
            .map(|secret| secret.expose().to_owned())
            .unwrap_or_default();

        self.password
            .update(cx, |input, cx| input.set_value(password, window, cx));
        self.fill(options, window, cx);
        self.refresh_name_placeholder(window, cx);
    }

    /// The name used when the field is left empty on a new connection.
    fn default_name(&self, cx: &App) -> Option<String> {
        if self.editing.is_some() {
            return None;
        }

        let address = match self.engine {
            EngineKind::Postgres | EngineKind::Mysql | EngineKind::Mariadb => {
                Self::text(&self.host, cx)
            }
            EngineKind::Sqlite => Self::text(&self.sqlite_path, cx),
        };
        default_name(self.engine, &address)
    }

    fn refresh_name_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = self
            .default_name(cx)
            .unwrap_or_else(|| NAME_PLACEHOLDER.to_owned());
        self.name.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
    }

    fn load_existing(
        &mut self,
        connection: ConnectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let manifest = Arc::clone(self.model.read(cx).manifest());
        let Some(alias) = manifest.connection(connection).cloned() else {
            return;
        };

        self.engine = alias.engine;
        self.name.update(cx, |input, cx| {
            input.set_value(alias.name.clone(), window, cx)
        });

        let load = self.model.read(cx).resolve(connection, cx);

        cx.spawn_in(window, async move |this, cx| {
            let Ok(binding) = load.await else {
                return;
            };

            this.update_in(cx, |this, window, cx| {
                this.has_stored_password = binding.credentials.password.is_some();
                if this.has_stored_password {
                    this.password.update(cx, |input, cx| {
                        input.set_placeholder(STORED_PASSWORD_PLACEHOLDER, window, cx)
                    });
                }
                this.fill(binding.descriptor.options, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn fill(&mut self, options: ConnectionOptions, window: &mut Window, cx: &mut Context<Self>) {
        let set = |input: &Entity<InputState>,
                   value: String,
                   window: &mut Window,
                   cx: &mut Context<Self>| {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        };

        let (host, port, user, database, tls) = match options {
            ConnectionOptions::Postgres(options) => (
                options.host,
                options.port,
                options.user,
                options.database,
                options.tls,
            ),
            ConnectionOptions::Mysql(options) | ConnectionOptions::Mariadb(options) => (
                options.host,
                options.port,
                options.user,
                options.database,
                options.tls,
            ),
            ConnectionOptions::Sqlite(options) => {
                set(
                    &self.sqlite_path,
                    options.path.display().to_string(),
                    window,
                    cx,
                );
                self.enforce_foreign_keys = options.enforce_foreign_keys;
                return;
            }
        };
        set(&self.host, host, window, cx);
        set(&self.port, port.to_string(), window, cx);
        set(&self.user, user, window, cx);
        set(&self.database, database, window, cx);
        let (index, ca_file) = match tls {
            TlsMode::Disable => (2, None),
            TlsMode::VerifyFull { ca_file: None } => (0, None),
            TlsMode::VerifyFull {
                ca_file: Some(path),
            } => (1, Some(path)),
        };
        self.tls_select.update(cx, |select, cx| {
            select.set_selected_index(Some(IndexPath::new(index)), window, cx)
        });
        if let Some(path) = ca_file {
            set(&self.ca_file, path.display().to_string(), window, cx);
        }
    }

    fn tls(&self, cx: &App) -> Tls {
        match self
            .tls_select
            .read(cx)
            .selected_index(cx)
            .map(|index| index.row)
        {
            Some(1) => Tls::VerifyWithCa,
            Some(2) => Tls::Disabled,
            _ => Tls::Verify,
        }
    }

    fn text(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().trim().to_owned()
    }

    fn options(&self, cx: &App) -> Result<ConnectionOptions, SharedString> {
        match self.engine {
            EngineKind::Postgres | EngineKind::Mysql | EngineKind::Mariadb => {
                // A parsed string has already filled the fields read below.
                if self.use_connection_string {
                    match &self.connection_string_state {
                        ConnectionStringState::Empty => {
                            return Err(MISSING_CONNECTION_STRING.into());
                        }
                        ConnectionStringState::Invalid(error) => return Err(error.clone()),
                        ConnectionStringState::Applied => {}
                    }
                }

                let port = Self::text(&self.port, cx)
                    .parse::<u16>()
                    .map_err(|_| SharedString::from("Enter a port between 1 and 65535"))?;
                let tls = match self.tls(cx) {
                    Tls::Verify => TlsMode::VerifyFull { ca_file: None },
                    Tls::VerifyWithCa => TlsMode::VerifyFull {
                        ca_file: Some(PathBuf::from(Self::text(&self.ca_file, cx))),
                    },
                    Tls::Disabled => TlsMode::Disable,
                };

                if matches!(self.engine, EngineKind::Mysql | EngineKind::Mariadb) {
                    let options = MysqlOptions {
                        host: Self::text(&self.host, cx),
                        port,
                        user: Self::text(&self.user, cx),
                        database: Self::text(&self.database, cx),
                        tls,
                        connect_timeout: Some(CONNECT_TIMEOUT),
                    };
                    return Ok(if self.engine == EngineKind::Mariadb {
                        ConnectionOptions::Mariadb(options)
                    } else {
                        ConnectionOptions::Mysql(options)
                    });
                }
                Ok(ConnectionOptions::Postgres(PostgresOptions {
                    host: Self::text(&self.host, cx),
                    port,
                    user: Self::text(&self.user, cx),
                    database: Self::text(&self.database, cx),
                    tls,
                    connect_timeout: Some(CONNECT_TIMEOUT),
                    application_name: None,
                }))
            }
            EngineKind::Sqlite => Ok(ConnectionOptions::Sqlite(SqliteOptions {
                enforce_foreign_keys: self.enforce_foreign_keys,
                ..SqliteOptions::new(Self::text(&self.sqlite_path, cx))
            })),
        }
    }

    /// Validates through the registered driver, so the form never encodes
    /// engine rules itself.
    fn validated(&self, cx: &App) -> Result<(ConnectionOptions, Option<Secret>), SharedString> {
        let options = self.options(cx)?;
        let drivers = Arc::clone(&self.model.read(cx).services().drivers);
        let driver = drivers
            .driver(options.engine())
            .map_err(|error| error_text(&error))?;

        if let Some(issue) = driver.validate(&options).first() {
            return Err(issue.message.clone().into());
        }

        let password = self.password.read(cx).value().to_string();
        let secret = (!password.is_empty()).then(|| Secret::new(password));
        Ok((options, secret))
    }

    fn set_status(
        &mut self,
        message: impl Into<SharedString>,
        is_error: bool,
        cx: &mut Context<Self>,
    ) {
        self.status = Some((message.into(), is_error));
        self.is_busy = false;
        cx.notify();
    }

    fn test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (options, secret) = match self.validated(cx) {
            Ok(validated) => validated,
            Err(error) => return self.set_status(error, true, cx),
        };

        let sessions = Arc::clone(&self.model.read(cx).services().sessions);
        let descriptor = ConnectionDescriptor {
            id: self.editing.unwrap_or_default(),
            name: Self::text(&self.name, cx),
            options,
        };
        let stored = self.stored_password(cx);

        self.is_busy = true;
        self.status = Some(("Connecting…".into(), false));
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let password = match secret {
                Some(secret) => Some(secret),
                None => match stored.await {
                    Ok(password) => password,
                    Err(error) => {
                        this.update(cx, |this, cx| this.set_status(error, true, cx))
                            .ok();
                        return;
                    }
                },
            };
            let result = sessions
                .test_connection(descriptor, Credentials { password })
                .await;

            this.update(cx, |this, cx| match result {
                Ok(Some(version)) => {
                    this.set_status(format!("Connected · version {version}"), false, cx)
                }
                Ok(None) => this.set_status("Connected", false, cx),
                Err(error) => this.set_status(error.message, true, cx),
            })
            .ok();
        })
        .detach();
    }

    /// The stored password when editing and the field was left empty.
    fn stored_password(&self, cx: &App) -> Task<Result<Option<Secret>, SharedString>> {
        let Some(connection) = self.editing else {
            return Task::ready(Ok(None));
        };
        let storage = self.model.read(cx).storage();
        cx.background_spawn(async move {
            storage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .stored_password(connection)
                .map_err(|error| error_text(&error))
        })
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (options, secret) = match self.validated(cx) {
            Ok(validated) => validated,
            Err(error) => return self.set_status(error, true, cx),
        };

        let typed_name = Self::text(&self.name, cx);
        let name = if typed_name.is_empty() {
            self.default_name(cx).unwrap_or_default()
        } else {
            typed_name
        };

        if name.is_empty() {
            return self.set_status("Enter a name for the connection", true, cx);
        }

        let engine = options.engine();
        let editing = self.editing;
        let stored = self.stored_password(cx);
        let model = self.model.clone();

        let alias = model.update(cx, |model, cx| {
            let alias_name = name.clone();
            model.update_workspace(cx, move |workspace| match editing {
                Some(connection) => workspace
                    .rename_connection(connection, &alias_name)
                    .map(|()| connection),
                None => workspace.add_connection(&alias_name, engine),
            })
        });

        self.is_busy = true;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let connection = match alias.await {
                Ok(connection) => connection,
                Err(error) => {
                    this.update(cx, |this, cx| this.set_status(error, true, cx))
                        .ok();
                    return;
                }
            };

            let password = match secret {
                Some(secret) => Some(secret),
                None => match stored.await {
                    Ok(password) => password,
                    Err(error) => {
                        this.update(cx, |this, cx| this.set_status(error, true, cx))
                            .ok();
                        return;
                    }
                },
            };

            let bind = model.update(cx, |model, cx| {
                model.bind_connection(connection, options, password, cx)
            });

            match bind.await {
                Ok(()) => {
                    model.update(cx, |model, cx| model.set_bound(connection, true, cx));
                    cx.update(|window, cx| window.close_dialog(cx)).ok();
                }
                Err(error) => {
                    this.update(cx, |this, cx| this.set_status(error, true, cx))
                        .ok();
                }
            }
        })
        .detach();
    }

    fn choose_sqlite_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };

            if let Some(path) = paths.into_iter().next() {
                this.update_in(cx, |this, window, cx| {
                    this.sqlite_path.update(cx, |input, cx| {
                        input.set_value(path.display().to_string(), window, cx)
                    });
                })
                .ok();
            }
        })
        .detach();
    }

    /// Creating a database file is its own action; opening never creates.
    fn create_sqlite_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self.model.read(cx).directory();
        let prompt = cx.prompt_for_new_path(&directory, Some(NEW_SQLITE_FILE_NAME));

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };

            let create_path = path.clone();
            let created = cx
                .background_spawn(async move { qwerio_sqlite::create_database_file(&create_path) })
                .await;

            this.update_in(cx, |this, window, cx| match created {
                Ok(()) => {
                    this.sqlite_path.update(cx, |input, cx| {
                        input.set_value(path.display().to_string(), window, cx)
                    });
                    this.set_status("Created an empty database file", false, cx);
                }
                Err(error) => this.set_status(error.message, true, cx),
            })
            .ok();
        })
        .detach();
    }
}

/// `postgres@localhost` for a server, `sqlite@app.db` for a file.
fn default_name(engine: EngineKind, address: &str) -> Option<String> {
    let prefix = match engine {
        EngineKind::Postgres => "postgres",
        EngineKind::Sqlite => "sqlite",
        EngineKind::Mysql => "mysql",
        EngineKind::Mariadb => "mariadb",
    };
    let target = match engine {
        EngineKind::Postgres | EngineKind::Mysql | EngineKind::Mariadb => Some(address),
        EngineKind::Sqlite => Path::new(address)
            .file_name()
            .and_then(|name| name.to_str()),
    };

    target
        .filter(|target| !target.is_empty())
        .map(|target| format!("{prefix}@{target}"))
}

pub(crate) fn field(label: &'static str, control: impl IntoElement, cx: &App) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(control)
}

/// The optional title and description shown above a step's content.
fn render_step_heading(
    title: Option<SharedString>,
    description: &'static str,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .gap_0p5()
        .children(title.map(|title| div().text_base().font_semibold().child(title)))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
}

impl Render for ConnectionForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.step {
            Step::ChooseEngine => self.render_engine_picker(cx).into_any_element(),
            Step::Details => self.render_details(cx).into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_a_postgres_connection_after_its_host() {
        assert_eq!(
            default_name(EngineKind::Postgres, "localhost").as_deref(),
            Some("postgres@localhost")
        );
    }

    #[test]
    fn names_a_sqlite_connection_after_its_file() {
        assert_eq!(
            default_name(EngineKind::Sqlite, "/data/app.db").as_deref(),
            Some("sqlite@app.db")
        );
    }

    #[test]
    fn has_no_default_without_an_address() {
        assert_eq!(default_name(EngineKind::Postgres, ""), None);
        assert_eq!(default_name(EngineKind::Sqlite, ""), None);
    }
}
