//! Changes staged in the dialog. Nothing runs until they are applied, and
//! staging the opposite of a pending change cancels it instead.

use qwerio_core::permissions::{PermissionChange, PermissionScope, Principal};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PendingChange {
    Privilege {
        principal: Principal,
        scope: PermissionScope,
        privilege: String,
        revoke: bool,
        grant_option: bool,
    },
    Membership {
        role: Principal,
        member: Principal,
        revoke: bool,
    },
    DefaultRole {
        role: Principal,
        member: Principal,
    },
}

impl PendingChange {
    pub(super) fn to_change(&self) -> PermissionChange {
        match self.clone() {
            Self::Privilege {
                principal,
                scope,
                privilege,
                revoke,
                grant_option,
            } => PermissionChange::Privilege {
                principal,
                scope,
                privilege,
                revoke,
                grant_option,
            },
            Self::Membership {
                role,
                member,
                revoke,
            } => PermissionChange::Membership {
                role,
                member,
                revoke,
            },
            Self::DefaultRole { role, member } => PermissionChange::DefaultRole { role, member },
        }
    }

    /// Whether both change the same permission, membership, or default,
    /// whichever direction they change it in.
    fn targets_same(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Privilege {
                    principal,
                    scope,
                    privilege,
                    ..
                },
                Self::Privilege {
                    principal: other_principal,
                    scope: other_scope,
                    privilege: other_privilege,
                    ..
                },
            ) => {
                principal == other_principal && scope == other_scope && privilege == other_privilege
            }
            (
                Self::Membership { role, member, .. },
                Self::Membership {
                    role: other_role,
                    member: other_member,
                    ..
                },
            )
            | (
                Self::DefaultRole { role, member },
                Self::DefaultRole {
                    role: other_role,
                    member: other_member,
                },
            ) => role == other_role && member == other_member,
            _ => false,
        }
    }
}

/// `changes` with `change` staged, or with its pending counterpart removed.
pub(super) fn toggled(changes: &[PendingChange], change: PendingChange) -> Vec<PendingChange> {
    let has_counterpart = changes.iter().any(|staged| staged.targets_same(&change));

    if has_counterpart {
        return changes
            .iter()
            .filter(|staged| !staged.targets_same(&change))
            .cloned()
            .collect();
    }

    changes.iter().cloned().chain([change]).collect()
}

/// `changes` with every pending grant to `principal` on `scope` set to
/// `grant_option`.
pub(super) fn with_grant_option(
    changes: &[PendingChange],
    principal: &Principal,
    scope: &PermissionScope,
    grant_option: bool,
) -> Vec<PendingChange> {
    changes
        .iter()
        .map(|change| match change {
            PendingChange::Privilege {
                principal: staged_principal,
                scope: staged_scope,
                privilege,
                revoke: false,
                ..
            } if staged_principal == principal && staged_scope == scope => {
                PendingChange::Privilege {
                    principal: principal.clone(),
                    scope: scope.clone(),
                    privilege: privilege.clone(),
                    revoke: false,
                    grant_option,
                }
            }
            _ => change.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use qwerio_core::permissions::ScopeKind;

    use super::*;

    fn alice() -> Principal {
        Principal {
            name: "alice".to_owned(),
            host: None,
        }
    }

    fn table() -> PermissionScope {
        PermissionScope {
            kind: ScopeKind::Table,
            database: "app".to_owned(),
            namespace: "public".to_owned(),
            object: "items".to_owned(),
            column: String::new(),
        }
    }

    fn privilege(name: &str, revoke: bool) -> PendingChange {
        PendingChange::Privilege {
            principal: alice(),
            scope: table(),
            privilege: name.to_owned(),
            revoke,
            grant_option: false,
        }
    }

    #[test]
    fn staging_the_opposite_change_cancels_the_pending_one() {
        let staged = toggled(&[], privilege("SELECT", false));
        let staged = toggled(&staged, privilege("INSERT", false));

        let staged = toggled(&staged, privilege("SELECT", true));

        assert_eq!(staged, [privilege("INSERT", false)]);
    }

    #[test]
    fn memberships_and_defaults_for_the_same_role_are_separate_changes() {
        let role = Principal {
            name: "readers".to_owned(),
            host: None,
        };
        let membership = PendingChange::Membership {
            role: role.clone(),
            member: alice(),
            revoke: false,
        };
        let default = PendingChange::DefaultRole {
            role,
            member: alice(),
        };

        let staged = toggled(std::slice::from_ref(&membership), default.clone());

        assert_eq!(staged, [membership, default]);
    }

    #[test]
    fn grant_option_applies_only_to_pending_grants_for_that_scope() {
        let other_scope = PermissionScope {
            object: "orders".to_owned(),
            ..table()
        };
        let elsewhere = PendingChange::Privilege {
            principal: alice(),
            scope: other_scope,
            privilege: "SELECT".to_owned(),
            revoke: false,
            grant_option: false,
        };
        let staged = [
            privilege("SELECT", false),
            privilege("DELETE", true),
            elsewhere.clone(),
        ];

        let updated = with_grant_option(&staged, &alice(), &table(), true);

        let grantable = PendingChange::Privilege {
            principal: alice(),
            scope: table(),
            privilege: "SELECT".to_owned(),
            revoke: false,
            grant_option: true,
        };
        assert_eq!(updated, [grantable, privilege("DELETE", true), elsewhere]);
    }
}
