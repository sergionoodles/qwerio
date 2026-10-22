//! Sorting the server's users and roles into the groups the sidebar shows.

use qwerio_core::connection::EngineKind;
use qwerio_core::permissions::{Principal, PrincipalInfo};

const ROLE_DESCRIPTIONS: [&str; 2] = ["Role", "Locked account / role"];
const POSTGRES_SYSTEM_PREFIX: &str = "pg_";
const MYSQL_SYSTEM_PREFIXES: [&str; 2] = ["mysql.", "mariadb."];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Group {
    Users,
    Roles,
    System,
}

impl Group {
    pub(super) const ALL: [Self; 3] = [Self::Users, Self::Roles, Self::System];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Users => "Users",
            Self::Roles => "Roles",
            Self::System => "Built-in",
        }
    }
}

pub(super) fn is_role(info: &PrincipalInfo) -> bool {
    ROLE_DESCRIPTIONS.contains(&info.description.as_str())
}

/// Accounts the server creates for itself, which few people manage.
fn is_system(engine: EngineKind, principal: &Principal) -> bool {
    match engine {
        EngineKind::Postgres => principal.name.starts_with(POSTGRES_SYSTEM_PREFIX),
        _ => MYSQL_SYSTEM_PREFIXES
            .iter()
            .any(|prefix| principal.name.starts_with(prefix)),
    }
}

pub(super) fn group_of(engine: EngineKind, info: &PrincipalInfo) -> Group {
    if is_system(engine, &info.principal) {
        return Group::System;
    }

    if is_role(info) {
        return Group::Roles;
    }

    Group::Users
}

/// The members of `group` whose label contains `query`, ignoring case.
pub(super) fn members<'a>(
    engine: EngineKind,
    principals: &'a [PrincipalInfo],
    group: Group,
    query: &str,
) -> Vec<&'a PrincipalInfo> {
    let query = query.trim().to_lowercase();

    principals
        .iter()
        .filter(|info| group_of(engine, info) == group)
        .filter(|info| info.principal.label().to_lowercase().contains(&query))
        .collect()
}

pub(super) fn find<'a>(principals: &'a [PrincipalInfo], label: &str) -> Option<&'a PrincipalInfo> {
    principals
        .iter()
        .find(|info| info.principal.label() == label)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, description: &str) -> PrincipalInfo {
        PrincipalInfo {
            principal: Principal {
                name: name.to_owned(),
                host: None,
            },
            description: description.to_owned(),
        }
    }

    fn names(infos: Vec<&PrincipalInfo>) -> Vec<&str> {
        infos
            .into_iter()
            .map(|info| info.principal.name.as_str())
            .collect()
    }

    #[test]
    fn built_in_accounts_are_kept_apart_from_users_and_roles() {
        let principals = [
            info("alice", "User"),
            info("readers", "Role"),
            info("pg_monitor", "Role"),
        ];

        let engine = EngineKind::Postgres;

        assert_eq!(
            names(members(engine, &principals, Group::Users, "")),
            ["alice"]
        );
        assert_eq!(
            names(members(engine, &principals, Group::Roles, "")),
            ["readers"]
        );
        assert_eq!(
            names(members(engine, &principals, Group::System, "")),
            ["pg_monitor"]
        );
    }

    #[test]
    fn mysql_locked_accounts_are_roles_and_internal_accounts_are_built_in() {
        let principals = [
            info("app", "User"),
            info("writers", "Locked account / role"),
            info("mysql.sys", "Locked account / role"),
        ];

        let engine = EngineKind::Mysql;

        assert_eq!(
            names(members(engine, &principals, Group::Roles, "")),
            ["writers"]
        );
        assert_eq!(
            names(members(engine, &principals, Group::System, "")),
            ["mysql.sys"]
        );
    }

    #[test]
    fn search_ignores_case_and_surrounding_spaces() {
        let principals = [info("Alice", "User"), info("bob", "User")];

        let found = members(EngineKind::Postgres, &principals, Group::Users, " ALI ");

        assert_eq!(names(found), ["Alice"]);
    }
}
