//! The roles a user or role belongs to, and a role's members.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div,
};
use qwerio_core::permissions::RoleMembership;

use super::directory;
use super::view::{card, muted};
use super::*;

/// Which side of a membership the shown identity is on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    /// The roles the identity belongs to.
    Member,
    /// The members of the identity, which is a role.
    Role,
}

impl Side {
    fn id_prefix(self) -> &'static str {
        match self {
            Self::Member => "permission-membership",
            Self::Role => "permission-member",
        }
    }

    fn empty_text(self) -> &'static str {
        match self {
            Self::Member => "Not a member of any role.",
            Self::Role => "No members yet.",
        }
    }

    /// The identity's own label and the other party's in `membership`.
    fn split(self, membership: &RoleMembership) -> (&str, &str) {
        match self {
            Self::Member => (&membership.member, &membership.role),
            Self::Role => (&membership.role, &membership.member),
        }
    }

    fn change(self, identity: &Principal, other: &Principal, revoke: bool) -> PendingChange {
        let (role, member) = match self {
            Self::Member => (other.clone(), identity.clone()),
            Self::Role => (identity.clone(), other.clone()),
        };
        PendingChange::Membership {
            role,
            member,
            revoke,
        }
    }
}

/// A membership as it will be once the staged changes are applied.
struct MembershipState {
    /// The label of the role or member on the other side.
    other: String,
    is_member: bool,
    is_pending: bool,
}

impl PermissionsForm {
    fn find_principal(&self, label: &str) -> Option<Principal> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| directory::find(&snapshot.principals, label))
            .map(|info| info.principal.clone())
    }

    /// The other party of every staged membership change for `identity`.
    fn staged_memberships(&self, identity: &Principal, side: Side) -> Vec<(String, bool)> {
        self.pending
            .iter()
            .filter_map(|change| match change {
                PendingChange::Membership {
                    role,
                    member,
                    revoke,
                } => {
                    let (own, other) = match side {
                        Side::Member => (member, role),
                        Side::Role => (role, member),
                    };
                    (own == identity).then(|| (other.label(), *revoke))
                }
                _ => None,
            })
            .collect()
    }

    fn membership_states(&self, identity: &Principal, side: Side) -> Vec<MembershipState> {
        let label = identity.label();
        let staged = self.staged_memberships(identity, side);
        let is_staged = |other: &str| staged.iter().any(|(staged, _)| staged == other);
        let current: Vec<_> = self
            .snapshot
            .as_ref()
            .map(PermissionSnapshot::role_memberships)
            .unwrap_or_default()
            .iter()
            .map(|membership| side.split(membership))
            .filter(|(own, _)| *own == label)
            .map(|(_, other)| MembershipState {
                is_member: true,
                is_pending: is_staged(other),
                other: other.to_owned(),
            })
            .collect();
        let added = staged
            .iter()
            .filter(|(_, revoke)| !revoke)
            .map(|(other, _)| MembershipState {
                other: other.clone(),
                is_member: false,
                is_pending: true,
            });

        current.into_iter().chain(added).collect()
    }

    /// MySQL and MariaDB roles stay inactive at sign-in unless made default.
    fn render_default_role(
        &self,
        id: &SharedString,
        identity: &Principal,
        role: &Principal,
        cx: &mut Context<Self>,
    ) -> Option<Button> {
        if self.engine == EngineKind::Postgres {
            return None;
        }
        let change = PendingChange::DefaultRole {
            role: role.clone(),
            member: identity.clone(),
        };
        let label = if self.pending.contains(&change) {
            "Undo activate at sign-in"
        } else {
            "Activate at sign-in"
        };
        Some(
            Button::new(SharedString::from(format!("{id}-default")))
                .ghost()
                .xsmall()
                .label(label)
                .tooltip("Make this role active by default when the account signs in")
                .disabled(self.is_busy)
                .on_click(cx.listener(move |this, _, _, cx| this.stage(change.clone(), cx))),
        )
    }

    fn render_membership(
        &self,
        id: SharedString,
        identity: &Principal,
        state: MembershipState,
        side: Side,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let other = self.find_principal(&state.other);
        let status = match (state.is_member, state.is_pending) {
            (true, true) => Some(Tag::danger().small().child("will be removed")),
            (false, _) => Some(Tag::info().small().child("will be added")),
            _ => None,
        };
        let is_settled = state.is_member && !state.is_pending;
        let default_role = other
            .as_ref()
            .filter(|_| side == Side::Member && is_settled)
            .and_then(|role| self.render_default_role(&id, identity, role, cx));
        let toggle = other.map(|other| {
            let change = side.change(identity, &other, state.is_member);
            let label = if state.is_pending { "Undo" } else { "Remove" };
            Button::new(SharedString::from(format!("{id}-toggle")))
                .ghost()
                .xsmall()
                .label(label)
                .disabled(self.is_busy)
                .on_click(cx.listener(move |this, _, _, cx| this.stage(change.clone(), cx)))
        });

        h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(state.other),
            )
            .children(status)
            .children(default_role)
            .children(toggle)
            .into_any_element()
    }

    pub(super) fn render_memberships(
        &self,
        identity: &Principal,
        side: Side,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let states = self.membership_states(identity, side);
        if states.is_empty() {
            return muted(side.empty_text(), cx).into_any_element();
        }
        let border = cx.theme().border;
        let rows: Vec<_> = states
            .into_iter()
            .enumerate()
            .map(|(index, state)| {
                let id = SharedString::from(format!("{}-{index}", side.id_prefix()));
                div()
                    .when(index > 0, |row| row.border_t_1().border_color(border))
                    .child(self.render_membership(id, identity, state, side, cx))
            })
            .collect();
        card(cx).children(rows).into_any_element()
    }

    pub(super) fn render_add_role(
        &self,
        identity: &Principal,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = identity.label();
        let taken: Vec<_> = self
            .membership_states(identity, Side::Member)
            .into_iter()
            .map(|state| state.other)
            .collect();
        let roles: Vec<Principal> = self
            .snapshot
            .iter()
            .flat_map(|snapshot| snapshot.principals.iter())
            .filter(|info| directory::is_role(info))
            .map(|info| info.principal.clone())
            .filter(|role| role.label() != label && !taken.contains(&role.label()))
            .collect();
        let this = cx.entity().downgrade();
        let member = identity.clone();

        Button::new("permission-add-role")
            .outline()
            .small()
            .icon(LucideIcon::Plus)
            .label("Add to role")
            .disabled(self.is_busy || roles.is_empty())
            .dropdown_menu(move |menu, _, _| {
                roles.iter().fold(menu, |menu, role| {
                    let this = this.clone();
                    let change = Side::Member.change(&member, role, false);
                    menu.item(PopupMenuItem::new(role.label()).on_click(move |_, _, cx| {
                        this.update(cx, |this, cx| this.stage(change.clone(), cx))
                            .ok();
                    }))
                })
            })
    }
}
