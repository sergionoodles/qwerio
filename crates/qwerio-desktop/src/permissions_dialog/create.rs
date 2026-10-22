//! Creating a user or a role. Creation runs right away, unlike staged changes,
//! so the new identity can be opened and given permissions.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{Disableable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, IntoElement as _, ParentElement as _, SharedString, Styled as _,
};

use super::view::{heading, muted, sql_block};
use super::*;
use crate::connection_dialog::field;

impl PermissionsForm {
    fn has_host_field(&self) -> bool {
        match self.engine {
            EngineKind::Mysql => true,
            EngineKind::Mariadb => self.pane == Pane::NewUser,
            _ => false,
        }
    }

    pub(super) fn render_create(&self, cx: &mut Context<Self>) -> AnyElement {
        let is_role = self.pane == Pane::NewRole;
        let (icon, title, description, action) = if is_role {
            (
                LucideIcon::ShieldUser,
                "New role",
                "A role bundles permissions. Add users to it to give them those permissions.",
                "Create role",
            )
        } else {
            (
                LucideIcon::UserPlus,
                "New user",
                "A user is an account that can sign in to the server.",
                "Create user",
            )
        };
        let host_help = if is_role {
            "MySQL identifies roles by name and host. % is the usual choice."
        } else {
            "Where the user may connect from. Use % to allow any host."
        };
        let preview = self
            .create_change(cx)
            .map(|(change, _)| self.preview(&change));
        let is_valid = matches!(preview, Some(Ok(_)));
        let sql: SharedString = match preview {
            Some(Ok(sql)) => sql.into(),
            Some(Err(message)) => message.into(),
            None => SharedString::default(),
        };

        v_flex()
            .gap_5()
            .child(heading(icon, title, None, description, cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(field(
                        "Name",
                        Input::new(&self.name)
                            .id("permission-name")
                            .disabled(self.is_busy),
                        cx,
                    ))
                    .when(self.has_host_field(), |form| {
                        form.child(
                            v_flex()
                                .gap_1()
                                .child(field(
                                    "Host",
                                    Input::new(&self.host)
                                        .id("permission-host")
                                        .disabled(self.is_busy),
                                    cx,
                                ))
                                .child(muted(host_help, cx)),
                        )
                    })
                    .when(!is_role, |form| {
                        form.child(field(
                            "Password",
                            Input::new(&self.password)
                                .id("permission-password")
                                .disabled(self.is_busy),
                            cx,
                        ))
                    }),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(muted("SQL to run", cx))
                    .child(sql_block("permission-create-preview", sql, is_valid, cx)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("permission-create-cancel")
                            .ghost()
                            .label("Cancel")
                            .disabled(self.is_busy)
                            .on_click(cx.listener(|this, _, _, cx| this.show(Pane::Overview, cx))),
                    )
                    .child(
                        Button::new("permission-create")
                            .primary()
                            .label(action)
                            .loading(self.is_busy)
                            .disabled(self.is_busy || !is_valid)
                            .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                    ),
            )
            .into_any_element()
    }
}
