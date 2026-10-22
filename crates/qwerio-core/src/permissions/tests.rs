use super::*;

fn principal(engine: EngineKind, name: &str) -> Principal {
    Principal {
        name: name.to_owned(),
        host: (engine == EngineKind::Mysql).then(|| "localhost".to_owned()),
    }
}

fn scope(kind: ScopeKind) -> PermissionScope {
    PermissionScope {
        kind,
        database: "app".to_owned(),
        namespace: "public".to_owned(),
        object: "items".to_owned(),
        column: "name".to_owned(),
    }
}

#[test]
fn creates_users_without_exposing_passwords_in_previews_or_debug() {
    for engine in [EngineKind::Postgres, EngineKind::Mysql, EngineKind::Mariadb] {
        let user = Principal {
            name: "ali'ce`\"".to_owned(),
            host: (engine != EngineKind::Postgres).then(|| "local`host".to_owned()),
        };
        let change = PermissionChange::CreateUser {
            principal: user,
            password: Secret::new("secret'\\word"),
        };
        let sql = change.render(engine).expect("statement");
        assert!(sql.starts_with("CREATE USER "));
        assert!(sql.contains("secret''"));
        assert!(!change.preview(engine).expect("preview").contains("secret"));
        assert!(!format!("{change:?}").contains("secret"));
    }
}

#[test]
fn quotes_postgres_identifiers_and_escape_literals_independently() {
    let change = PermissionChange::CreateUser {
        principal: principal(EngineKind::Postgres, "x\"; DROP ROLE admin; --"),
        password: Secret::new("p'\\end"),
    };
    assert_eq!(
        change.render(EngineKind::Postgres).expect("sql"),
        "CREATE USER \"x\"\"; DROP ROLE admin; --\" WITH LOGIN PASSWORD E'p''\\\\end'"
    );
}

#[test]
fn renders_database_table_column_and_routine_grants_for_each_engine() {
    for engine in [EngineKind::Postgres, EngineKind::Mysql, EngineKind::Mariadb] {
        for kind in PermissionScope::available_kinds(engine) {
            let target = scope(*kind);
            for privilege in target.privileges(engine) {
                let change = PermissionChange::Privilege {
                    principal: principal(engine, "reader"),
                    scope: target.clone(),
                    privilege: (*privilege).to_owned(),
                    revoke: false,
                    grant_option: true,
                };
                assert!(
                    change
                        .render(engine)
                        .expect("grant")
                        .ends_with(" WITH GRANT OPTION")
                );
                assert!(
                    grants_query(engine, &target)
                        .expect("catalog")
                        .starts_with("SELECT ")
                );
            }
        }
    }
    let change = PermissionChange::Privilege {
        principal: principal(EngineKind::Postgres, "reader"),
        scope: scope(ScopeKind::Column),
        privilege: "UPDATE".to_owned(),
        revoke: true,
        grant_option: false,
    };
    assert_eq!(
        change.render(EngineKind::Postgres).expect("revoke"),
        "REVOKE UPDATE (\"name\") ON TABLE \"public\".\"items\" FROM \"reader\""
    );
}

#[test]
fn rejects_unsupported_scopes_injected_privileges_and_missing_names() {
    let mut change = PermissionChange::Privilege {
        principal: principal(EngineKind::Postgres, "reader"),
        scope: scope(ScopeKind::Table),
        privilege: "SELECT; DROP TABLE items".to_owned(),
        revoke: false,
        grant_option: false,
    };
    assert!(change.render(EngineKind::Postgres).is_err());
    if let PermissionChange::Privilege {
        scope, privilege, ..
    } = &mut change
    {
        *privilege = "SELECT".to_owned();
        scope.object.clear();
    }
    assert!(change.render(EngineKind::Postgres).is_err());
    assert!(grants_query(EngineKind::Mysql, &scope(ScopeKind::Schema)).is_err());
    assert!(grants_query(EngineKind::Postgres, &scope(ScopeKind::Server)).is_err());
    assert!(principals_query(EngineKind::Sqlite, false).is_err());
}

#[test]
fn roles_and_memberships_preserve_engine_account_semantics() {
    for engine in [EngineKind::Postgres, EngineKind::Mysql, EngineKind::Mariadb] {
        let role = principal(engine, "readers");
        assert!(
            PermissionChange::CreateRole { role: role.clone() }
                .render(engine)
                .expect("role")
                .starts_with("CREATE ROLE ")
        );
        for revoke in [false, true] {
            let change = PermissionChange::Membership {
                role: role.clone(),
                member: principal(engine, "alice"),
                revoke,
            };
            let sql = change.render(engine).expect("membership");
            assert!(sql.starts_with(if revoke { "REVOKE " } else { "GRANT " }));
            assert!(sql.contains(if revoke { " FROM " } else { " TO " }));
        }
    }
    let missing_host = PermissionChange::CreateRole {
        role: principal(EngineKind::Postgres, "readers"),
    };
    assert!(missing_host.render(EngineKind::Mysql).is_err());
    let wrong_host = PermissionChange::CreateRole {
        role: principal(EngineKind::Mysql, "readers"),
    };
    assert!(wrong_host.render(EngineKind::Mariadb).is_err());
}

#[test]
fn version_gates_roles_without_confusing_mariadb_compatibility_prefix() {
    assert!(!supports_roles(EngineKind::Mysql, "5.7.44"));
    assert!(supports_roles(EngineKind::Mysql, "8.0.39-commercial"));
    assert!(supports_roles(EngineKind::Mariadb, "5.5.5-10.11.8-MariaDB"));
    assert!(!supports_roles(EngineKind::Mariadb, "10.0.4-MariaDB"));
    assert!(!supports_roles(EngineKind::Mysql, "unknown"));
    assert!(!supports_roles(EngineKind::Sqlite, "3.50.0"));
}

#[test]
fn rejects_empty_passwords_nuls_and_sqlite_changes() {
    let user = principal(EngineKind::Postgres, "alice");
    let empty = PermissionChange::CreateUser {
        principal: user,
        password: Secret::new(""),
    };
    assert!(empty.preview(EngineKind::Postgres).is_err());
    let nul = PermissionChange::CreateRole {
        role: principal(EngineKind::Postgres, "bad\0name"),
    };
    assert!(nul.render(EngineKind::Postgres).is_err());
    assert!(nul.render(EngineKind::Sqlite).is_err());
}

#[test]
fn database_grants_escape_wildcards_without_broadening_access() {
    let scope = PermissionScope {
        database: "sales_2026%".to_owned(),
        ..scope(ScopeKind::Database)
    };
    for engine in [EngineKind::Mysql, EngineKind::Mariadb] {
        let change = PermissionChange::Privilege {
            principal: Principal {
                name: "reader".to_owned(),
                host: Some("localhost".to_owned()),
            },
            scope: scope.clone(),
            privilege: "SELECT".to_owned(),
            revoke: false,
            grant_option: false,
        };
        assert!(
            change
                .render(engine)
                .expect("escaped database")
                .contains("`sales\\_2026\\%`.*")
        );
        assert!(
            change
                .render_for_server(engine, true)
                .expect("literal database")
                .contains("`sales_2026%`.*")
        );
        let table = PermissionChange::Privilege {
            principal: Principal {
                name: "reader".to_owned(),
                host: Some("localhost".to_owned()),
            },
            scope: PermissionScope {
                kind: ScopeKind::Table,
                ..scope.clone()
            },
            privilege: "SELECT".to_owned(),
            revoke: false,
            grant_option: false,
        };
        assert!(
            table
                .render(engine)
                .expect("literal qualifier")
                .contains("`sales_2026%`.`items`")
        );
    }
}

#[test]
fn default_role_activation_uses_the_engines_syntax() {
    for (engine, preposition) in [(EngineKind::Mysql, " TO "), (EngineKind::Mariadb, " FOR ")] {
        let change = PermissionChange::DefaultRole {
            role: principal(engine, "readers"),
            member: Principal {
                name: "alice".to_owned(),
                host: Some("localhost".to_owned()),
            },
        };
        assert!(
            change
                .render(engine)
                .expect("default role")
                .starts_with("SET DEFAULT ROLE ")
        );
        assert!(
            change
                .render(engine)
                .expect("default role")
                .contains(preposition)
        );
        assert!(change.render(EngineKind::Postgres).is_err());
    }
}

#[test]
fn postgres_public_grants_address_the_pseudo_role() {
    let change = PermissionChange::Privilege {
        principal: Principal {
            name: "PUBLIC".to_owned(),
            host: None,
        },
        scope: scope(ScopeKind::Function),
        privilege: "EXECUTE".to_owned(),
        revoke: true,
        grant_option: false,
    };
    assert_eq!(
        change.render(EngineKind::Postgres).expect("public revoke"),
        "REVOKE EXECUTE ON FUNCTION \"public\".\"items\" FROM PUBLIC"
    );
}

#[test]
fn legacy_account_discovery_does_not_require_role_columns() {
    for engine in [EngineKind::Mysql, EngineKind::Mariadb] {
        let query = principals_query(engine, false).expect("legacy query");
        assert!(!query.contains("is_role"));
        assert!(!query.contains("account_locked"));
        assert!(query.contains("User, Host"));
    }
}
