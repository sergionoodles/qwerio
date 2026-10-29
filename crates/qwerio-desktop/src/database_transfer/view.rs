use gpui_kit::component::WindowExt as _;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::Input;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, StyledExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    InteractiveElement as _, IntoElement, Render, StatefulInteractiveElement as _,
    TestSupportExt as _, div,
};

use super::*;

impl TransferForm {
    fn render_target(&self, cx: &App) -> impl IntoElement {
        let connection = self
            .model
            .read(cx)
            .manifest()
            .connection(self.connection)
            .map(|alias| alias.name.clone())
            .unwrap_or_default();
        let detail = match &self.options {
            Some(ConnectionOptions::Sqlite(options)) => options.path.display().to_string(),
            Some(ConnectionOptions::Postgres(options)) => format!(
                "{} · {}@{}:{}",
                self.database.as_deref().unwrap_or_default(),
                options.user,
                options.host,
                options.port
            ),
            Some(ConnectionOptions::Mysql(options) | ConnectionOptions::Mariadb(options)) => {
                format!(
                    "{} · {}@{}:{}",
                    self.database.as_deref().unwrap_or_default(),
                    options.user,
                    options.host,
                    options.port
                )
            }
            None => "Loading connection…".to_owned(),
        };
        v_flex()
            .gap_1()
            .child(
                div()
                    .font_semibold()
                    .child(format!("{} · {connection}", self.engine.display_name())),
            )
            .child(
                div()
                    .id("transfer-target")
                    .text_sm()
                    .aria_label(detail.clone())
                    .child(detail)
                    .test_support(),
            )
    }

    fn render_file(&self, is_restored_file: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, input, id) = if is_restored_file {
            (
                "Restore into a new SQLite file",
                &self.restored_file,
                "transfer-restored-file",
            )
        } else if self.operation == Operation::Backup {
            ("Save backup to a new file", &self.file, "transfer-file")
        } else {
            ("Backup file", &self.file, "transfer-file")
        };
        v_flex().gap_1().child(div().text_sm().child(label)).child(
            h_flex()
                .gap_2()
                .child(
                    div().flex_1().child(
                        Input::new(input)
                            .id(id)
                            .aria_label(label)
                            .disabled(self.is_running),
                    ),
                )
                .child(
                    Button::new(if is_restored_file {
                        "transfer-choose-destination"
                    } else {
                        "transfer-choose-file"
                    })
                    .outline()
                    .label("Choose…")
                    .disabled(self.is_running)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_file(is_restored_file, window, cx)
                    })),
                ),
        )
    }

    fn render_format(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let buttons = [
            (Format::Archive, "Custom archive", "transfer-format-archive"),
            (Format::Sql, "SQL script", "transfer-format-sql"),
        ]
        .into_iter()
        .map(|(format, label, id)| {
            Button::new(id)
                .outline()
                .label(label)
                .selected(self.format == format)
                .disabled(self.is_running || self.is_checking)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.format = format;
                    this.has_confirmed_restore = false;
                    this.check_tools(window, cx);
                }))
        });
        v_flex()
            .gap_1()
            .child(div().text_sm().child("Backup format"))
            .child(h_flex().gap_2().children(buttons))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("For restore, choose the format of the backup file."),
            )
    }

    fn render_tools(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = if self.is_checking {
            "Checking native tools…".to_owned()
        } else if let Some(tool) = &self.tool {
            format!("{}\n{}", tool.version, tool.path.display())
        } else {
            self.tool_error
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "Check the tools folder before continuing.".to_owned())
        };
        v_flex()
            .gap_2()
            .child(div().text_sm().child("Native tools folder"))
            .child(
                Input::new(&self.tool_directory)
                    .id("transfer-tool-directory")
                    .aria_label("Native tools folder")
                    .disabled(self.is_running || self.is_checking),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("transfer-locate-tools")
                            .outline()
                            .label("Locate Tools…")
                            .disabled(self.is_running || self.is_checking)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.locate_tools(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("transfer-check-tools")
                            .outline()
                            .label("Check Again")
                            .disabled(self.is_running || self.is_checking)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.check_tools(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .id("transfer-tool-status")
                    .text_xs()
                    .text_color(if self.tool_error.is_some() {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .aria_label(status.clone())
                    .child(status)
                    .test_support(),
            )
    }

    fn render_confirmation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let message = if self.engine == EngineKind::Sqlite {
            "Restore creates a new database file. The connected database is unchanged."
        } else {
            "Restore writes to the database shown above. Existing objects may be replaced, and an error or cancellation can leave partial changes. Only restore backups you trust."
        };
        v_flex()
            .gap_2()
            .child(Alert::warning("transfer-restore-warning", message).small())
            .child(
                Checkbox::new("transfer-confirm-restore")
                    .label(if self.engine == EngineKind::Sqlite {
                        "Create the restored database file"
                    } else {
                        "I confirm this restore target"
                    })
                    .checked(self.has_confirmed_restore)
                    .disabled(self.is_running)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.has_confirmed_restore = *checked;
                        cx.notify();
                    })),
            )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let action = match self.operation {
            Operation::Backup => "Back Up",
            Operation::Restore => "Restore",
        };
        let progress = match self.operation {
            Operation::Backup => "Backing up…",
            Operation::Restore => "Restoring…",
        };
        h_flex()
            .w_full()
            .gap_2()
            .child(
                Button::new("transfer-close")
                    .outline()
                    .label("Close")
                    .disabled(self.is_running)
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(div().flex_1())
            .when(self.is_running, |view| {
                view.child(div().text_sm().child(progress)).child(
                    Button::new("transfer-cancel")
                        .outline()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancelled.store(true, Ordering::Relaxed);
                            cx.notify();
                        })),
                )
            })
            .when(!self.is_running, |view| {
                view.child(
                    Button::new("transfer-start")
                        .primary()
                        .label(action)
                        .disabled(!self.can_start(cx))
                        .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                )
            })
    }
}

impl Render for TransferForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("database-transfer-dialog")
            .gap_4()
            .child(self.render_target(cx))
            .when(self.engine == EngineKind::Postgres, |view| {
                view.child(self.render_format(cx))
            })
            .child(self.render_file(false, cx))
            .when(
                self.engine == EngineKind::Sqlite && self.operation == Operation::Restore,
                |view| view.child(self.render_file(true, cx)),
            )
            .when(self.engine != EngineKind::Sqlite, |view| {
                view.child(self.render_tools(cx))
            })
            .when(self.operation == Operation::Restore, |view| {
                view.child(self.render_confirmation(cx))
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .id("transfer-error")
                        .aria_label(error.clone())
                        .child(Alert::error("transfer-error-alert", error).small())
                        .test_support(),
                )
            })
            .when_some(self.notice.clone(), |view, notice| {
                view.child(
                    div()
                        .id("transfer-notice")
                        .aria_label(notice.clone())
                        .child(Alert::success("transfer-success-alert", notice).small())
                        .test_support(),
                )
            })
            .child(self.render_footer(cx))
            .into_any_element()
    }
}
