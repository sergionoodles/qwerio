//! Creating or editing a connection. The workspace manifest only receives the
//! connection's name and engine; the address or file stays in this
//! computer's local state, and the password goes to the platform keychain.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogClose;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IndexPath, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, PathPromptOptions,
    Render, SharedString, Styled as _, Subscription, Window, div,
};
use qwerio_core::connection::{
    ConnectionDescriptor, ConnectionOptions, Credentials, DEFAULT_POSTGRES_PORT, EngineKind,
    PostgresOptions, Secret, SqliteOptions, TlsMode,
};
use qwerio_core::ids::ConnectionId;

use crate::workspace_model::{WorkspaceModel, error_text};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const ENGINES: [EngineKind; 2] = [EngineKind::Postgres, EngineKind::Sqlite];
const TLS_LABELS: [&str; 3] = ["Verify certificate", "Verify with CA file", "No encryption"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tls {
    Verify,
    VerifyWithCa,
    Disabled,
}

pub struct ConnectionForm {
    model: Entity<WorkspaceModel>,
    editing: Option<ConnectionId>,
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
                    .gap_2()
                    .justify_end()
                    .child(
                        DialogClose::new()
                            .child(Button::new("cancel-connection").ghost().label("Cancel")),
                    )
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

        let subscriptions = vec![
            cx.subscribe(
                &engine_select,
                |_, _, _: &SelectEvent<Vec<SharedString>>, cx| cx.notify(),
            ),
            cx.subscribe(
                &tls_select,
                |_, _, _: &SelectEvent<Vec<SharedString>>, cx| cx.notify(),
            ),
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
            name: input("Name shown in the workspace", window, cx),
            host: input("Host or socket directory", window, cx),
            port,
            user: input("User", window, cx),
            database: input("Database", window, cx),
            password,
            ca_file: input("CA certificate file (PEM)", window, cx),
            sqlite_path: input("Database file", window, cx),
            model,
            editing,
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

    fn load_existing(
        &mut self,
        connection: ConnectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let model = self.model.read(cx);
        let manifest = Arc::clone(model.manifest());
        let services = Arc::clone(model.services());
        let Some(alias) = manifest.connection(connection).cloned() else {
            return;
        };

        let engine_index = ENGINES
            .iter()
            .position(|engine| *engine == alias.engine)
            .unwrap_or_default();
        self.engine_select.update(cx, |select, cx| {
            select.set_selected_index(Some(IndexPath::new(engine_index)), window, cx)
        });
        self.name.update(cx, |input, cx| {
            input.set_value(alias.name.clone(), window, cx)
        });

        let load = cx
            .background_spawn(async move { services.local_state.binding(manifest.id, connection) });

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Some(binding)) = load.await else {
                return;
            };

            this.update_in(cx, |this, window, cx| {
                this.has_stored_password = binding.credential.is_some();
                this.fill(binding.options, window, cx);
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
                None => stored.await,
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
    fn stored_password(&self, cx: &App) -> gpui_kit::Task<Option<Secret>> {
        let (Some(connection), true) = (self.editing, self.has_stored_password) else {
            return gpui_kit::Task::ready(None);
        };

        let model = self.model.read(cx);
        let credentials = Arc::clone(&model.services().credentials);
        let reference =
            qwerio_app::credentials::CredentialRef::for_connection(model.manifest().id, connection);

        cx.background_spawn(async move { credentials.load(&reference).ok().flatten() })
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (options, secret) = match self.validated(cx) {
            Ok(validated) => validated,
            Err(error) => return self.set_status(error, true, cx),
        };

        let name = Self::text(&self.name, cx);

        if name.is_empty() {
            return self.set_status("Enter a name for the connection", true, cx);
        }

        let engine = options.engine();
        let editing = self.editing;
        let stored = self.stored_password(cx);
        let model = self.model.clone();
        let services = Arc::clone(model.read(cx).services());

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
                None => stored.await,
            };

            let manifest = model.read_with(cx, |model, _| Arc::clone(model.manifest()));

            let bind = cx.background_spawn(async move {
                services
                    .bind_connection(&manifest, connection, options, password)
                    .map_err(|error| error_text(&error))
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
        let directory = self.model.read(cx).root().clone();
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

fn field(label: &'static str, control: impl IntoElement, cx: &App) -> impl IntoElement {
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
            .child(field("Name", Input::new(&self.name), cx))
            .when(self.editing.is_none(), |this| {
                this.child(field("Engine", Select::new(&self.engine_select), cx))
            })
            .when(engine == EngineKind::Postgres, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(field("Host", Input::new(&self.host), cx)))
                        .child(div().w_24().child(field("Port", Input::new(&self.port), cx))),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(field("User", Input::new(&self.user), cx)))
                        .child(div().flex_1().child(field("Database", Input::new(&self.database), cx))),
                )
                .child(field("Password", Input::new(&self.password), cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(password_placeholder),
                )
                .child(field("Encryption", Select::new(&self.tls_select), cx))
                .when(tls == Tls::VerifyWithCa, |this| {
                    this.child(field("CA certificate", Input::new(&self.ca_file), cx))
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
                        .child(div().flex_1().child(Input::new(&self.sqlite_path)))
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
                        .text_sm()
                        .text_color(if is_error { cx.theme().danger } else { cx.theme().success })
                        .child(message),
                )
            })
    }
}
