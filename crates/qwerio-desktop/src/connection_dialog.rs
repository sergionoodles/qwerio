//! Creating or editing a connection. File workspaces save connection details
//! and passwords inside the workspace's optional encryption. Legacy folders
//! retain their machine-local bindings and platform keyring credentials.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogClose;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IndexPath, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div,
};
use qwerio_core::connection::{
    ConnectionDescriptor, ConnectionOptions, Credentials, DEFAULT_POSTGRES_PORT, EngineKind,
    PostgresOptions, Secret, SqliteOptions, TlsMode,
};
use qwerio_core::connection_string::{ParsedConnectionString, parse_connection_string};
use qwerio_core::ids::ConnectionId;

use crate::workspace_model::{WorkspaceModel, error_text};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const ENGINES: [EngineKind; 2] = [EngineKind::Postgres, EngineKind::Sqlite];
const TLS_LABELS: [&str; 3] = ["Verify certificate", "Verify with CA file", "No encryption"];
const NAME_PLACEHOLDER: &str = "Name shown in the workspace";
const CONNECTION_STRING_PLACEHOLDER: &str = "postgres://user:password@host:5432/database";
const CONNECTION_STRING_APPLIED: &str = "Filled in from the connection string";
const NEW_SQLITE_FILE_NAME: &str = "database.sqlite";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tls {
    Verify,
    VerifyWithCa,
    Disabled,
}

pub struct ConnectionForm {
    model: Entity<WorkspaceModel>,
    editing: Option<ConnectionId>,
    connection_string: Entity<InputState>,
    /// The outcome of reading the connection string, and whether it failed.
    connection_string_feedback: Option<(SharedString, bool)>,
    engine_select: Entity<SelectState<Vec<SharedString>>>,
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

    window.open_dialog(cx, move |dialog, _, cx| {
        let form = form.clone();
        let is_busy = form.read(cx).is_busy;
        let test_form = form.clone();
        let save_form = form.clone();

        dialog
            .title(title)
            .w(gpui_kit::px(520.))
            .child(form.clone())
            .footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_end()
                    .child(
                        div().flex_none().child(
                            DialogClose::new()
                                .child(Button::new("cancel-connection").ghost().label("Cancel")),
                        ),
                    )
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
                    ),
            )
    });
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
        let engine_labels: Vec<SharedString> = ENGINES
            .iter()
            .map(|engine| engine.display_name().into())
            .collect();
        let tls_labels: Vec<SharedString> = TLS_LABELS
            .iter()
            .map(|label| SharedString::from(*label))
            .collect();
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };

        let engine_select =
            cx.new(|cx| SelectState::new(engine_labels, Some(IndexPath::new(0)), window, cx));
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
            cx.subscribe_in(
                &engine_select,
                window,
                |this, _, _: &SelectEvent<Vec<SharedString>>, window, cx| {
                    this.refresh_name_placeholder(window, cx);
                    cx.notify();
                },
            ),
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
                .placeholder("Password")
                .masked(true)
        });

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
            connection_string,
            connection_string_feedback: None,
            engine_select,
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
        self.connection_string_feedback = if text.is_empty() {
            None
        } else {
            match parse_connection_string(&text) {
                Ok(parsed) => {
                    self.apply_connection_string(parsed, window, cx);
                    Some((CONNECTION_STRING_APPLIED.into(), false))
                }
                Err(error) => Some((error.to_string().into(), true)),
            }
        };
        cx.notify();
    }

    fn apply_connection_string(
        &mut self,
        parsed: ParsedConnectionString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let password = parsed
            .password
            .as_ref()
            .map(|secret| secret.expose().to_owned())
            .unwrap_or_default();

        self.select_engine(parsed.options.engine(), window, cx);
        self.password
            .update(cx, |input, cx| input.set_value(password, window, cx));
        self.fill(parsed.options, window, cx);
        self.refresh_name_placeholder(window, cx);
    }

    fn select_engine(&mut self, engine: EngineKind, window: &mut Window, cx: &mut Context<Self>) {
        let index = ENGINES
            .iter()
            .position(|candidate| *candidate == engine)
            .unwrap_or_default();
        self.engine_select.update(cx, |select, cx| {
            select.set_selected_index(Some(IndexPath::new(index)), window, cx)
        });
    }

    /// The name used when the field is left empty on a new connection.
    fn default_name(&self, cx: &App) -> Option<String> {
        if self.editing.is_some() {
            return None;
        }

        let address = match self.engine(cx) {
            EngineKind::Postgres => Self::text(&self.host, cx),
            EngineKind::Sqlite => Self::text(&self.sqlite_path, cx),
        };
        default_name(self.engine(cx), &address)
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
        let model = self.model.read(cx);
        let manifest = Arc::clone(model.manifest());
        let Some(alias) = manifest.connection(connection).cloned() else {
            return;
        };

        self.select_engine(alias.engine, window, cx);
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

        match options {
            ConnectionOptions::Postgres(options) => {
                set(&self.host, options.host, window, cx);
                set(&self.port, options.port.to_string(), window, cx);
                set(&self.user, options.user, window, cx);
                set(&self.database, options.database, window, cx);

                let (tls, ca_file) = match options.tls {
                    TlsMode::Disable => (Tls::Disabled, None),
                    TlsMode::VerifyFull { ca_file: None } => (Tls::Verify, None),
                    TlsMode::VerifyFull {
                        ca_file: Some(path),
                    } => (Tls::VerifyWithCa, Some(path)),
                };
                let index = match tls {
                    Tls::Verify => 0,
                    Tls::VerifyWithCa => 1,
                    Tls::Disabled => 2,
                };
                self.tls_select.update(cx, |select, cx| {
                    select.set_selected_index(Some(IndexPath::new(index)), window, cx)
                });

                if let Some(path) = ca_file {
                    set(&self.ca_file, path.display().to_string(), window, cx);
                }
            }
            ConnectionOptions::Sqlite(options) => {
                set(
                    &self.sqlite_path,
                    options.path.display().to_string(),
                    window,
                    cx,
                );
                self.enforce_foreign_keys = options.enforce_foreign_keys;
            }
        }
    }

    fn engine(&self, cx: &App) -> EngineKind {
        self.engine_select
            .read(cx)
            .selected_index(cx)
            .and_then(|index| ENGINES.get(index.row).copied())
            .unwrap_or(EngineKind::Postgres)
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
        match self.engine(cx) {
            EngineKind::Postgres => {
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

        let password = Self::text(&self.password, cx);
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
        let prompt = cx.prompt_for_new_path(&directory, Some("database.sqlite"));

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
    };
    let target = match engine {
        EngineKind::Postgres => Some(address),
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

impl Render for ConnectionForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let engine = self.engine(cx);
        let tls = self.tls(cx);
        let password_placeholder = if self.has_stored_password {
            "Leave empty to keep the saved password"
        } else {
            "Password"
        };

        v_flex()
            .gap_3()
            .when(self.editing.is_none(), |this| {
                this.child(field(
                    "Connection string",
                    Input::new(&self.connection_string)
                        .id("connection-string")
                        .aria_label("Connection string")
                        .mask_toggle(),
                    cx,
                ))
                .when_some(self.connection_string_feedback.clone(), |this, (message, is_error)| {
                    this.child(
                        div()
                            .id("connection-string-feedback")
                            .text_xs()
                            .text_color(if is_error { cx.theme().danger } else { cx.theme().muted_foreground })
                            .aria_label(message.clone())
                            .child(message)
                            .test_support(),
                    )
                })
            })
            .child(field("Name", Input::new(&self.name).id("connection-name").aria_label("Name"), cx))
            .when(self.editing.is_none(), |this| {
                this.child(field("Engine", Select::new(&self.engine_select).id("connection-engine").accessibility_label("Engine"), cx))
            })
            .when(engine == EngineKind::Postgres, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(field("Host", Input::new(&self.host).id("connection-host").aria_label("Host"), cx)))
                        .child(div().w_24().child(field("Port", Input::new(&self.port).id("connection-port").aria_label("Port"), cx))),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(field("User", Input::new(&self.user).id("connection-user").aria_label("User"), cx)))
                        .child(div().flex_1().child(field("Database", Input::new(&self.database).id("connection-database").aria_label("Database"), cx))),
                )
                .child(field("Password", Input::new(&self.password).id("connection-password").aria_label("Password"), cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(password_placeholder),
                )
                .child(field("Encryption", Select::new(&self.tls_select).id("connection-encryption").accessibility_label("Encryption"), cx))
                .when(tls == Tls::VerifyWithCa, |this| {
                    this.child(field("CA certificate", Input::new(&self.ca_file).id("connection-ca-file").aria_label("CA certificate"), cx))
                })
                .when(tls == Tls::Disabled, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child("Without encryption, passwords and data travel in plain text. Use only for local servers."),
                    )
                })
            })
            .when(engine == EngineKind::Sqlite, |this| {
                this.child(field(
                    "Database file",
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.sqlite_path).id("connection-file").aria_label("Database file")))
                        .child(
                            Button::new("choose-file")
                                .outline()
                                .small()
                                .label("Choose…")
                                .on_click(cx.listener(|this, _, window, cx| this.choose_sqlite_file(window, cx))),
                        )
                        .child(
                            Button::new("create-file")
                                .ghost()
                                .small()
                                .label("New File…")
                                .on_click(cx.listener(|this, _, window, cx| this.create_sqlite_file(window, cx))),
                        ),
                    cx,
                ))
                .child(
                    Checkbox::new("foreign-keys")
                        .label("Enforce foreign keys")
                        .checked(self.enforce_foreign_keys)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.enforce_foreign_keys = *checked;
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("The workspace shares only this connection's name. The address, file, and password stay on this computer."),
            )
            .when_some(self.status.clone(), |this, (message, is_error)| {
                this.child(
                    div()
                        .id("connection-status")
                        .text_sm()
                        .text_color(if is_error { cx.theme().danger } else { cx.theme().success })
                        .role(Role::Status)
                        .aria_label(message.clone())
                        .child(message)
                        .test_support(),
                )
            })
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
