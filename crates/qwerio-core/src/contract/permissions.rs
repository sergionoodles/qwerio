//! Runs only against disposable server fixtures, never SQLite or user servers.

use super::*;
use crate::connection::{EngineKind, Secret};
use crate::permissions::{self, PermissionChange, PermissionScope, Principal, ScopeKind};

pub async fn manages_users_roles_and_permissions(context: Context) {
    let admin = context.console().await;
    let engine = admin.info().engine;
    let suffix = crate::ids::ConnectionId::new()
        .as_uuid()
        .simple()
        .to_string();
    let user = Principal {
        name: format!("qwerio_u_'`{}", &suffix[..16]),
        host: (engine != EngineKind::Postgres).then(|| "%".to_owned()),
    };
    let role = Principal {
        name: format!("qwerio_r_{}", &suffix[..16]),
        host: (engine == EngineKind::Mysql).then(|| "%".to_owned()),
    };
    let password = "permission-test-';\\secret";
    let create = PermissionChange::CreateUser {
        principal: user.clone(),
        password: Secret::new(password),
    };
    permissions::apply(admin.as_ref(), &create)
        .await
        .expect("create user");
    let duplicate = permissions::apply(admin.as_ref(), &create)
        .await
        .expect_err("duplicate user");
    assert!(!format!("{duplicate:?}").contains("secret"));
    permissions::apply(
        admin.as_ref(),
        &PermissionChange::CreateRole { role: role.clone() },
    )
    .await
    .expect("create role");
    let membership = PermissionChange::Membership {
        role: role.clone(),
        member: user.clone(),
        revoke: false,
    };
    permissions::apply(admin.as_ref(), &membership)
        .await
        .expect("assign role");
    if engine != EngineKind::Postgres {
        permissions::apply(
            admin.as_ref(),
            &PermissionChange::DefaultRole {
                role: role.clone(),
                member: user.clone(),
            },
        )
        .await
        .expect("default role");
    }

    let scope = PermissionScope {
        kind: ScopeKind::Table,
        database: context.database.clone().expect("database"),
        namespace: "public".to_owned(),
        object: "items".to_owned(),
        column: "name".to_owned(),
    };
    let initial = permissions::inspect(admin.as_ref(), Some(&scope))
        .await
        .expect("inspect");
    assert!(initial.warnings.is_empty(), "{:?}", initial.warnings);
    assert!(initial.principals.iter().any(|info| info.principal == user));
    assert!(initial.principals.iter().any(|info| info.principal == role));
    assert!(
        initial
            .memberships
            .iter()
            .any(|row| row[0] == Value::Text(role.label()) && row[1] == Value::Text(user.label()))
    );

    exercise_user_access(&context, admin.as_ref(), &scope, &user, &role, password).await;
    let grant = |scope, privilege: &str, revoke| PermissionChange::Privilege {
        principal: role.clone(),
        scope,
        privilege: privilege.to_owned(),
        revoke,
        grant_option: false,
    };

    exercise_scopes(admin.as_ref(), &scope, &role).await;
    permissions::apply(admin.as_ref(), &grant(scope.clone(), "SELECT", true))
        .await
        .expect("revoke table");
    permissions::apply(
        admin.as_ref(),
        &PermissionChange::Membership {
            role: role.clone(),
            member: user.clone(),
            revoke: true,
        },
    )
    .await
    .expect("revoke membership");
    let snapshot = permissions::inspect(admin.as_ref(), Some(&scope))
        .await
        .expect("updated grants");
    assert!(
        !snapshot
            .memberships
            .iter()
            .any(|row| row[0] == Value::Text(role.label()) && row[1] == Value::Text(user.label()))
    );

    let readonly = context
        .open(SessionPurpose::Console, AccessMode::ReadOnly)
        .await;
    assert_eq!(
        permissions::apply(readonly.as_ref(), &create)
            .await
            .expect_err("readonly")
            .kind,
        ErrorKind::ReadOnly
    );
    close(readonly.as_ref()).await;
    for principal in [&user, &role] {
        let name = engine.quote_identifier(&principal.name);
        let account = principal.host.as_ref().map_or_else(
            || name.clone(),
            |host| format!("{name}@{}", engine.quote_identifier(host)),
        );
        let verb = if principal == &user || engine == EngineKind::Mysql {
            "USER"
        } else {
            "ROLE"
        };
        assert_eq!(
            failure(&run(admin.as_ref(), &format!("DROP {verb} {account}")).await),
            None
        );
    }
    close(admin.as_ref()).await;
}

fn reader_request(context: &Context, user: &Principal, password: &str) -> OpenSessionRequest {
    let mut connection = context.connection.clone();
    let database = match &mut connection.options {
        crate::connection::ConnectionOptions::Postgres(options) => {
            options.user = user.name.clone();
            context.database.clone()
        }
        crate::connection::ConnectionOptions::Mysql(options)
        | crate::connection::ConnectionOptions::Mariadb(options) => {
            options.user = user.name.clone();
            options.database.clear();
            None
        }
        _ => panic!("server fixture"),
    };
    OpenSessionRequest {
        connection,
        credentials: Credentials::password(password),
        purpose: SessionPurpose::Console,
        access: AccessMode::ReadWrite,
        database,
    }
}

async fn exercise_user_access(
    context: &Context,
    admin: &dyn DatabaseSession,
    scope: &PermissionScope,
    user: &Principal,
    role: &Principal,
    password: &str,
) {
    let engine = admin.info().engine;
    let namespace = if engine == EngineKind::Postgres {
        &scope.namespace
    } else {
        &scope.database
    };
    let table = format!(
        "{}.{}",
        engine.quote_identifier(namespace),
        engine.quote_identifier(&scope.object)
    );
    let reader = context
        .driver
        .open(reader_request(context, user, password))
        .await
        .expect("login with escaped password");
    assert_eq!(
        failure(&run(reader.as_ref(), &format!("SELECT name FROM {table}")).await)
            .map(|error| error.kind),
        Some(ErrorKind::Permission)
    );
    let denied = permissions::apply(
        reader.as_ref(),
        &PermissionChange::CreateRole {
            role: Principal {
                name: "qwerio_forbidden_role".to_owned(),
                host: role.host.clone(),
            },
        },
    )
    .await
    .expect_err("unprivileged role creation");
    assert_eq!(denied.kind, ErrorKind::Permission);

    let column_scope = PermissionScope {
        kind: ScopeKind::Column,
        ..scope.clone()
    };
    for revoke in [false, true] {
        permissions::apply(
            admin,
            &PermissionChange::Privilege {
                principal: user.clone(),
                scope: column_scope.clone(),
                privilege: "SELECT".to_owned(),
                revoke,
                grant_option: false,
            },
        )
        .await
        .expect("column grant/revoke");
        let events = run(reader.as_ref(), &format!("SELECT name FROM {table}")).await;
        assert_eq!(failure(&events).is_some(), revoke);
        assert!(
            failure(&run(reader.as_ref(), &format!("SELECT score FROM {table}")).await).is_some()
        );
    }

    let grant = |scope, privilege: &str, revoke| PermissionChange::Privilege {
        principal: role.clone(),
        scope,
        privilege: privilege.to_owned(),
        revoke,
        grant_option: false,
    };
    permissions::apply(admin, &grant(scope.clone(), "SELECT", false))
        .await
        .expect("grant through role");
    // MySQL-family privilege caches refresh at a fresh connection.
    close(reader.as_ref()).await;
    let reader = context
        .driver
        .open(reader_request(context, user, password))
        .await
        .expect("reader with role");
    assert_eq!(
        failure(&run(reader.as_ref(), &format!("SELECT * FROM {table}")).await),
        None
    );
    let snapshot = permissions::inspect(admin, Some(scope))
        .await
        .expect("grants");
    assert!(snapshot.grants.iter().any(|row| {
        row[0]
            .canonical_text()
            .is_some_and(|name| name.contains(&role.name))
            && row[1] == Value::Text("SELECT".to_owned())
    }));
    close(reader.as_ref()).await;
}

async fn exercise_scopes(admin: &dyn DatabaseSession, base: &PermissionScope, role: &Principal) {
    let engine = admin.info().engine;
    let kinds = PermissionScope::available_kinds(engine);
    for kind in kinds {
        if matches!(kind, ScopeKind::Table | ScopeKind::Column) {
            continue;
        }
        let mut scope = PermissionScope {
            kind: *kind,
            ..base.clone()
        };
        let setup = match kind {
            ScopeKind::Sequence => {
                scope.object = "permission_sequence".to_owned();
                Some("CREATE SEQUENCE permission_sequence")
            }
            ScopeKind::Function => {
                scope.object = "permission_function".to_owned();
                (engine == EngineKind::Postgres).then_some("CREATE FUNCTION permission_function() RETURNS integer LANGUAGE SQL AS 'SELECT 1'")
            }
            ScopeKind::Procedure => {
                scope.object = "permission_procedure".to_owned();
                (engine == EngineKind::Postgres)
                    .then_some("CREATE PROCEDURE permission_procedure() LANGUAGE SQL AS 'SELECT 1'")
            }
            _ => None,
        };
        if let Some(sql) = setup {
            assert_eq!(failure(&run(admin, sql).await), None, "{sql}");
        }
        let privilege = scope
            .privileges(engine)
            .first()
            .expect("privilege")
            .to_string();
        for revoke in [false, true] {
            let change = PermissionChange::Privilege {
                principal: role.clone(),
                scope: scope.clone(),
                privilege: privilege.clone(),
                revoke,
                grant_option: !revoke,
            };
            permissions::apply(admin, &change)
                .await
                .expect("scope mutation");
            let snapshot = permissions::inspect(admin, Some(&scope))
                .await
                .expect("scope inspection");
            assert!(snapshot.warnings.is_empty(), "{:?}", snapshot.warnings);
            let has_grant = snapshot.grants.iter().any(|row| {
                row[0]
                    .canonical_text()
                    .is_some_and(|name| name.contains(&role.name))
                    && row[1]
                        .canonical_text()
                        .is_some_and(|value| value.contains(&privilege))
                    && row[2] == Value::Text("YES".to_owned())
            });
            assert_eq!(has_grant, !revoke, "{engine:?} {kind:?}");
        }
    }
}
