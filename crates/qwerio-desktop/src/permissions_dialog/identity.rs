//! One user or role and what it may do on the chosen object.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, div,
};
use qwerio_core::permissions::{DirectGrant, PrincipalInfo};

use super::directory;
use super::memberships::Side;
use super::sidebar::principal_icon;
use super::view::{card, heading, muted, section};
use super::*;

const PRIVILEGE_COLUMNS_WIDTH: f32 = 1. / 3.;

/// What a privilege lets someone do, in plain words.
fn privilege_description(privilege: &str) -> &'static str {
    match privilege {
        "SELECT" => "Read data",
        "INSERT" => "Add rows",
        "UPDATE" => "Change rows",
        "DELETE" => "Remove rows",
        "TRUNCATE" => "Empty tables",
        "REFERENCES" => "Create foreign keys",
        "TRIGGER" => "Create triggers",
        "CONNECT" => "Connect to the database",
        "CREATE" => "Create objects",
        "TEMPORARY" | "CREATE TEMPORARY TABLES" => "Create temporary tables",
        "USAGE" => "Use and look up objects",
        "EXECUTE" => "Run functions and procedures",
        "ALTER" => "Change table structure",
        "DROP" => "Delete objects",
        "INDEX" => "Create and drop indexes",
        "CREATE VIEW" => "Create views",
        "SHOW VIEW" => "See view definitions",
        "CREATE ROUTINE" => "Create functions and procedures",
        "ALTER ROUTINE" => "Change or drop routines",
        "LOCK TABLES" => "Lock tables",
        "EVENT" => "Schedule events",
        "CREATE USER" => "Manage accounts",
        "SHOW DATABASES" => "List every database",
        _ => "",
    }
}

/// The current state of one privilege, counting staged changes.
struct PrivilegeState {
    privilege: &'static str,
    is_granted: bool,
    is_grantable: bool,
    is_pending: bool,
}

impl PrivilegeState {
    fn is_checked(&self) -> bool {
        self.is_granted != self.is_pending
    }
}

impl PermissionsForm {
    fn privilege_change(
        principal: &Principal,
        scope: &PermissionScope,
        privilege: &str,
        revoke: bool,
        grant_option: bool,
    ) -> PendingChange {
        PendingChange::Privilege {
            principal: principal.clone(),
            scope: scope.clone(),
            privilege: privilege.to_owned(),
            revoke,
            grant_option,
        }
    }

    /// Staged grants to `principal` on `scope` share one grant option.
    fn pending_grant_option(&self, principal: &Principal, scope: &PermissionScope) -> Option<bool> {
        self.pending.iter().find_map(|change| match change {
            PendingChange::Privilege {
                principal: target,
                scope: target_scope,
                revoke: false,
                grant_option,
                ..
            } if target == principal && target_scope == scope => Some(*grant_option),
            _ => None,
        })
    }

    fn privilege_states(
        &self,
        principal: &Principal,
        scope: &PermissionScope,
        grants: &[DirectGrant],
    ) -> Vec<PrivilegeState> {
        let label = principal.label();
        scope
            .privileges(self.engine)
            .iter()
            .map(|privilege| {
                let grant = grants
                    .iter()
                    .find(|grant| grant.grantee == label && grant.privilege == *privilege);
                let is_pending = self.pending.iter().any(|change| {
                    matches!(change, PendingChange::Privilege {
                        principal: target,
                        scope: target_scope,
                        privilege: target_privilege,
                        ..
                    } if target == principal && target_scope == scope && target_privilege == privilege)
                });
                PrivilegeState {
                    privilege,
                    is_granted: grant.is_some(),
                    is_grantable: grant.is_some_and(|grant| grant.grantable),
                    is_pending,
                }
            })
            .collect()
    }

    /// Stages `privilege` for `principal`, or cancels it if already staged.
    fn toggle_privilege(
        &mut self,
        principal: &Principal,
        scope: &PermissionScope,
        state: &PrivilegeState,
        cx: &mut Context<Self>,
    ) {
        let grant_option = self.pending_grant_option(principal, scope).unwrap_or(false);
        let change = Self::privilege_change(
            principal,
            scope,
            state.privilege,
            state.is_granted,
            grant_option,
        );
        self.stage(change, cx);
    }

    /// Stages every privilege that is not yet `checked`.
    fn set_all_privileges(&mut self, principal: &Principal, checked: bool, cx: &mut Context<Self>) {
        let Some(scope) = self.shown_scope() else {
            return;
        };
        let grants = self
            .snapshot
            .as_ref()
            .map(PermissionSnapshot::direct_grants)
            .unwrap_or_default();
        let states = self.privilege_states(principal, &scope, &grants);
        states
            .iter()
            .filter(|state| state.is_checked() != checked)
            .for_each(|state| self.toggle_privilege(principal, &scope, state, cx));
    }

    fn render_privilege(
        &self,
        index: usize,
        principal: &Principal,
        scope: &PermissionScope,
        state: PrivilegeState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let description = privilege_description(state.privilege);
        let status = match (state.is_pending, state.is_granted, state.is_grantable) {
            (true, true, _) => Some(("will be revoked", cx.theme().danger)),
            (true, false, _) => Some(("will be granted", cx.theme().primary)),
            (false, _, true) => Some(("can grant to others", cx.theme().muted_foreground)),
            _ => None,
        };
        let principal = principal.clone();
        let scope = scope.clone();
        let checkbox = Checkbox::new(("permission-privilege", index))
            .label(state.privilege)
            .checked(state.is_checked())
            .disabled(self.is_busy)
            .on_click(cx.listener(move |this, _: &bool, _, cx| {
                this.toggle_privilege(&principal, &scope, &state, cx)
            }));

        v_flex()
            .w(gpui_kit::relative(PRIVILEGE_COLUMNS_WIDTH))
            .pr_3()
            .py_1p5()
            .gap_0p5()
            .child(checkbox)
            .child(
                div()
                    .pl_6()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(description),
            )
            .children(
                status
                    .map(|(status, color)| div().pl_6().text_xs().text_color(color).child(status)),
            )
            .into_any_element()
    }

    fn render_privileges(&self, principal: &Principal, cx: &mut Context<Self>) -> AnyElement {
        let Some(scope) = self.shown_scope() else {
            return self.render_scope_hint(cx);
        };
        let grants = self
            .snapshot
            .as_ref()
            .map(PermissionSnapshot::direct_grants)
            .unwrap_or_default();
        let states = self.privilege_states(principal, &scope, &grants);
        let items: Vec<_> = states
            .into_iter()
            .enumerate()
            .map(|(index, state)| self.render_privilege(index, principal, &scope, state, cx))
            .collect();
        let public: Vec<_> = grants
            .iter()
            .filter(|grant| grant.grantee == "PUBLIC")
            .map(|grant| grant.privilege.clone())
            .collect();
        let grant_option = self.pending_grant_option(principal, &scope);
        let name = principal.name.clone();

        let bulk_action = |id: &'static str, label: &'static str, checked: bool| {
            let principal = principal.clone();
            Button::new(id)
                .ghost()
                .xsmall()
                .label(label)
                .disabled(self.is_busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_all_privileges(&principal, checked, cx)
                }))
        };

        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_1()
                    .child(bulk_action("permission-select-all", "Select all", true))
                    .child(bulk_action("permission-clear-all", "Clear all", false)),
            )
            .child(
                card(cx)
                    .id("permission-privileges")
                    .px_3()
                    .py_2()
                    .child(h_flex().flex_wrap().children(items)),
            )
            .when_some(grant_option, |view, grant_option| {
                let principal = principal.clone();
                let scope = scope.clone();
                view.child(
                    Checkbox::new("permission-grant-option")
                        .label(format!("Let {name} grant the new permissions to others"))
                        .checked(grant_option)
                        .disabled(self.is_busy)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.pending = pending::with_grant_option(
                                &this.pending,
                                &principal,
                                &scope,
                                *checked,
                            );
                            cx.notify();
                        })),
                )
            })
            .when(!public.is_empty(), |view| {
                view.child(muted(
                    format!("Everyone (PUBLIC) also has {} here.", public.join(", ")),
                    cx,
                ))
            })
            .child(muted(
                "Only direct grants are shown. Roles, ownership, and grants on the whole \
                 database or schema can also give access.",
                cx,
            ))
            .into_any_element()
    }

    fn identity_detail(&self, info: &PrincipalInfo) -> String {
        let kind = if directory::is_role(info) {
            "Grants its permissions to its members"
        } else {
            "Can sign in"
        };
        match info
            .principal
            .host
            .as_deref()
            .filter(|host| !host.is_empty())
        {
            Some(host) => format!("{kind} · connects from {host}"),
            None => kind.to_owned(),
        }
    }

    pub(super) fn render_identity(
        &self,
        principal: &Principal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(info) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| directory::find(&snapshot.principals, &principal.label()))
            .cloned()
        else {
            return self.render_scope_hint(cx);
        };
        let is_role = directory::is_role(&info);
        let tag = if is_role { "Role" } else { "User" };
        let scope_path = self.scope_path();

        v_flex()
            .gap_5()
            .child(heading(
                principal_icon(&info),
                principal.name.clone(),
                Some(tag),
                self.identity_detail(&info),
                cx,
            ))
            .child(
                section(
                    "Permissions",
                    format!("What {} may do on {scope_path}.", principal.name),
                    cx,
                )
                .child(self.render_privileges(principal, cx)),
            )
            .when(self.supports_roles(), |view| {
                view.child(
                    section(
                        "Member of",
                        "Roles pass their permissions on to their members.",
                        cx,
                    )
                    .child(self.render_memberships(principal, Side::Member, cx))
                    .child(h_flex().child(self.render_add_role(principal, cx))),
                )
                .when(is_role, |view| {
                    view.child(
                        section("Members", "Users and roles that receive this role.", cx)
                            .child(self.render_memberships(principal, Side::Role, cx)),
                    )
                })
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_privilege_is_explained() {
        for engine in [EngineKind::Postgres, EngineKind::Mysql, EngineKind::Mariadb] {
            for kind in PermissionScope::available_kinds(engine) {
                let scope = PermissionScope {
                    kind: *kind,
                    ..PermissionScope::server()
                };
                for privilege in scope.privileges(engine) {
                    assert!(
                        !privilege_description(privilege).is_empty(),
                        "{privilege} has no description"
                    );
                }
            }
        }
    }
}
