//! Step two of a new connection, and the whole dialog when editing: the
//! engine's settings, grouped in a card below the connection name.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::Input;
use gpui_kit::component::select::Select;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div, px,
};
use qwerio_core::connection::EngineKind;

use super::engine_picker::{engine_logo, engine_summary};
use super::{ConnectionForm, ConnectionStringState, Tls, field, render_step_heading};

const HEADER_LOGO_TILE: f32 = 40.;
const HEADER_LOGO: f32 = 22.;
const CONNECTION_STRING_HINT: &str = "Paste a URL, such as a DATABASE_URL value. Host, port, user, password, database, and encryption are read from it.";
const CONNECTION_STRING_APPLIED: &str =
    "Details filled in. Turn off \u{201c}Use connection string\u{201d} to review them.";
const PLAIN_TEXT_WARNING: &str =
    "Without encryption, passwords and data travel in plain text. Use only for local servers.";
const SHARING_NOTE: &str = "The workspace shares only this connection's name. The address, file, and password stay on this computer.";

impl ConnectionForm {
    pub(super) fn render_details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = match self.engine {
            EngineKind::Postgres | EngineKind::Mysql | EngineKind::Mariadb => {
                self.render_server(cx)
            }
            EngineKind::Sqlite => self.render_sqlite(cx),
        };

        v_flex()
            .gap_4()
            .child(self.render_header(cx))
            .child(field(
                "Name",
                Input::new(&self.name)
                    .id("connection-name")
                    .aria_label("Name"),
                cx,
            ))
            .child(settings)
            .when_some(self.status.clone(), |this, (message, is_error)| {
                this.child(render_status(message, is_error, cx))
            })
            .child(render_sharing_note(cx))
    }

    fn render_header(&self, cx: &App) -> impl IntoElement {
        let is_new = self.editing.is_none();
        let title = if is_new {
            format!("Connect to {}", self.engine.display_name())
        } else {
            self.engine.display_name().to_owned()
        };

        h_flex()
            .gap_3()
            .items_center()
            .child(engine_logo(
                self.engine,
                px(HEADER_LOGO_TILE),
                px(HEADER_LOGO),
                cx,
            ))
            .child(render_step_heading(
                Some(title.into()),
                engine_summary(self.engine),
                cx,
            ))
    }

    fn render_server(&self, cx: &mut Context<Self>) -> AnyElement {
        let toggle = Switch::new("use-connection-string")
            .label("Use connection string")
            .small()
            .checked(self.use_connection_string)
            .on_click(cx.listener(|this, checked: &bool, window, cx| {
                this.set_use_connection_string(*checked, window, cx)
            }));
        let content = if self.use_connection_string {
            self.render_connection_string(cx).into_any_element()
        } else {
            self.render_server_fields(cx).into_any_element()
        };

        let content = v_flex()
            .gap_3()
            .child(content)
            .child(
                Checkbox::new("show-all-databases")
                    .label("Show all databases")
                    .checked(self.show_all_databases)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.show_all_databases = *checked;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Turn off to show only the database specified in this connection."),
            );

        render_section(
            "Connection",
            Some(toggle.into_any_element()),
            content.into_any_element(),
            cx,
        )
    }

    fn render_connection_string(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let (message, color): (SharedString, _) = match &self.connection_string_state {
            ConnectionStringState::Empty => (CONNECTION_STRING_HINT.into(), theme.muted_foreground),
            ConnectionStringState::Applied => (CONNECTION_STRING_APPLIED.into(), theme.success),
            ConnectionStringState::Invalid(error) => (error.clone(), theme.danger),
        };

        v_flex()
            .gap_2()
            .child(
                Input::new(&self.connection_string)
                    .id("connection-string")
                    .aria_label("Connection string")
                    .mask_toggle(),
            )
            .child(
                div()
                    .id("connection-string-feedback")
                    .text_xs()
                    .text_color(color)
                    .aria_label(message.clone())
                    .child(message)
                    .test_support(),
            )
    }

    fn render_server_fields(&self, cx: &App) -> impl IntoElement {
        let tls = self.tls(cx);

        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div().flex_1().child(field(
                            "Host",
                            Input::new(&self.host)
                                .id("connection-host")
                                .aria_label("Host"),
                            cx,
                        )),
                    )
                    .child(
                        div().w_24().child(field(
                            "Port",
                            Input::new(&self.port)
                                .id("connection-port")
                                .aria_label("Port"),
                            cx,
                        )),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div().flex_1().child(field(
                            "User",
                            Input::new(&self.user)
                                .id("connection-user")
                                .aria_label("User"),
                            cx,
                        )),
                    )
                    .child(
                        div().flex_1().child(field(
                            "Password",
                            Input::new(&self.password)
                                .id("connection-password")
                                .aria_label("Password")
                                .mask_toggle(),
                            cx,
                        )),
                    ),
            )
            .child(field(
                "Database",
                Input::new(&self.database)
                    .id("connection-database")
                    .aria_label("Database"),
                cx,
            ))
            .child(field(
                "Encryption",
                Select::new(&self.tls_select)
                    .id("connection-encryption")
                    .accessibility_label("Encryption"),
                cx,
            ))
            .when(tls == Tls::VerifyWithCa, |this| {
                this.child(field(
                    "CA certificate",
                    Input::new(&self.ca_file)
                        .id("connection-ca-file")
                        .aria_label("CA certificate"),
                    cx,
                ))
            })
            .when(tls == Tls::Disabled, |this| {
                this.child(Alert::warning("plain-text-warning", PLAIN_TEXT_WARNING).small())
            })
    }

    fn render_sqlite(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div().flex_1().child(
                            Input::new(&self.sqlite_path)
                                .id("connection-file")
                                .aria_label("Database file"),
                        ),
                    )
                    .child(
                        Button::new("choose-file")
                            .outline()
                            .label("Choose…")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_sqlite_file(window, cx)
                            })),
                    )
                    .child(
                        Button::new("create-file")
                            .ghost()
                            .label("New File…")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.create_sqlite_file(window, cx)
                            })),
                    ),
            )
            .child(
                Checkbox::new("foreign-keys")
                    .label("Enforce foreign keys")
                    .checked(self.enforce_foreign_keys)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.enforce_foreign_keys = *checked;
                        cx.notify();
                    })),
            );

        render_section("Database file", None, content.into_any_element(), cx)
    }
}

/// A bordered card with a title row, which may carry a control.
fn render_section(
    title: &'static str,
    trailing: Option<AnyElement>,
    content: AnyElement,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();

    v_flex()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .px_4()
                .py_2p5()
                .gap_3()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.muted.opacity(0.4))
                .rounded_t_lg()
                .child(div().text_sm().font_semibold().child(title))
                .children(trailing),
        )
        .child(div().p_4().child(content))
        .into_any_element()
}

fn render_status(message: SharedString, is_error: bool, cx: &App) -> impl IntoElement {
    let (icon, color) = if is_error {
        (IconName::CircleX, cx.theme().danger)
    } else {
        (IconName::CircleCheck, cx.theme().success)
    };

    h_flex()
        .id("connection-status")
        .gap_2()
        .items_start()
        .text_sm()
        .text_color(color)
        .role(Role::Status)
        .aria_label(message.clone())
        .child(Icon::new(icon).small().mt_0p5())
        .child(div().flex_1().child(message))
        .test_support()
}

fn render_sharing_note(cx: &App) -> impl IntoElement {
    h_flex()
        .gap_2()
        .items_start()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(LucideIcon::Lock).xsmall().mt_0p5())
        .child(div().flex_1().child(SHARING_NOTE))
}
