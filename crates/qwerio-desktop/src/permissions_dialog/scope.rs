//! The object whose permissions are shown, and who has access to it.

use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div, px,
};
use qwerio_core::permissions::DirectGrant;

use super::view::{card, muted, section};
use super::*;

const PUBLIC_GRANTEE: &str = "PUBLIC";
const PATH_SEPARATOR: &str = " › ";
const GRANTEE_COLUMN_WIDTH: f32 = 180.;

pub(super) fn grantee_name(grantee: &str) -> String {
    if grantee == PUBLIC_GRANTEE {
        return "Everyone (PUBLIC)".to_owned();
    }
    grantee.to_owned()
}

/// Each grantee with its privileges, in the order the server listed them.
fn grants_by_grantee(grants: &[DirectGrant]) -> Vec<(String, Vec<&DirectGrant>)> {
    grants.iter().fold(Vec::new(), |mut groups, grant| {
        match groups
            .iter_mut()
            .find(|(grantee, _)| *grantee == grant.grantee)
        {
            Some((_, privileges)) => privileges.push(grant),
            None => groups.push((grant.grantee.clone(), vec![grant])),
        }
        groups
    })
}

impl PermissionsForm {
    fn is_object_scope(&self) -> bool {
        matches!(
            self.scope.kind,
            ScopeKind::Table
                | ScopeKind::Column
                | ScopeKind::Sequence
                | ScopeKind::Function
                | ScopeKind::Procedure
        )
    }

    fn has_schema(&self) -> bool {
        self.engine == EngineKind::Postgres && self.scope.kind != ScopeKind::Database
    }

    pub(super) fn scope_path(&self) -> String {
        if self.scope.kind == ScopeKind::Server {
            return "every database on the server".to_owned();
        }
        let scope = &self.scope;
        let object = match scope.kind {
            ScopeKind::Column => format!("{}.{}", scope.object, scope.column),
            _ => scope.object.clone(),
        };
        [
            Some(scope.database.clone()),
            self.has_schema().then(|| scope.namespace.clone()),
            self.is_object_scope().then_some(object),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(PATH_SEPARATOR)
    }

    /// Explains why grants are missing when the scope was not read.
    pub(super) fn render_scope_hint(&self, cx: &App) -> AnyElement {
        let hint = if self.is_busy {
            "Reading permissions…"
        } else {
            "Permissions could not be read from the server."
        };
        muted(hint, cx).into_any_element()
    }

    fn render_grantee_name(&self, grantee: &str, cx: &mut Context<Self>) -> AnyElement {
        let name = grantee_name(grantee);
        let principal = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| super::directory::find(&snapshot.principals, grantee))
            .map(|info| info.principal.clone());
        let Some(principal) = principal else {
            return div().truncate().child(name).into_any_element();
        };
        let link = cx.theme().link;

        div()
            .id(SharedString::from(format!("permission-grantee-{grantee}")))
            .truncate()
            .text_color(link)
            .cursor_pointer()
            .hover(|style| style.underline())
            .aria_label(name.clone())
            .child(name)
            .on_click(
                cx.listener(move |this, _, _, cx| this.show(Pane::Identity(principal.clone()), cx)),
            )
            .test_support()
            .into_any_element()
    }

    fn render_grantee_row(
        &self,
        grantee: &str,
        grants: &[&DirectGrant],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tags = grants.iter().map(|grant| {
            let tag = Tag::secondary().small().child(grant.privilege.clone());
            if grant.grantable {
                return tag.outline();
            }
            tag
        });

        h_flex()
            .px_3()
            .py_2()
            .gap_3()
            .items_start()
            .text_sm()
            .child(
                div()
                    .w(px(GRANTEE_COLUMN_WIDTH))
                    .flex_none()
                    .min_w_0()
                    .child(self.render_grantee_name(grantee, cx)),
            )
            .child(h_flex().flex_1().flex_wrap().gap_1().children(tags))
            .into_any_element()
    }

    pub(super) fn render_overview(&self, cx: &mut Context<Self>) -> AnyElement {
        let grants = self
            .snapshot
            .as_ref()
            .map(PermissionSnapshot::direct_grants)
            .unwrap_or_default();
        let groups = grants_by_grantee(&grants);
        let border = cx.theme().border;
        let body = if self.shown_scope().is_none() {
            self.render_scope_hint(cx)
        } else if groups.is_empty() {
            muted(
                "Nobody has a direct grant here. Select a user or role to grant access.",
                cx,
            )
            .into_any_element()
        } else {
            let rows: Vec<_> = groups
                .iter()
                .enumerate()
                .map(|(index, (grantee, grants))| {
                    div()
                        .when(index > 0, |row| row.border_t_1().border_color(border))
                        .child(self.render_grantee_row(grantee, grants, cx))
                })
                .collect();
            card(cx)
                .id("permission-grants")
                .children(rows)
                .into_any_element()
        };

        v_flex()
            .child(
                section(
                    "Who has access",
                    format!(
                        "Direct grants on {}. Outlined privileges can be passed on to others. \
                         Select someone to change their access.",
                        self.scope_path()
                    ),
                    cx,
                )
                .child(body),
            )
            .into_any_element()
    }
}
