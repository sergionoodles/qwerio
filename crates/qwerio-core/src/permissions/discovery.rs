use super::render::literal;
use super::*;

/// Columns: name, nullable host, description. No authentication data is read.
pub fn principals_query(engine: EngineKind, has_roles: bool) -> DbResult<String> {
    ensure_supported(engine)?;
    if !has_roles && engine != EngineKind::Postgres {
        return Ok("SELECT User, Host, 'User' FROM mysql.user ORDER BY User, Host".to_owned());
    }
    Ok(match engine {
        EngineKind::Postgres => "SELECT rolname::text, NULL::text, CASE WHEN rolcanlogin THEN 'User' ELSE 'Role' END::text FROM pg_roles ORDER BY rolname".to_owned(),
        EngineKind::Mysql => "SELECT User, Host, CASE WHEN account_locked = 'Y' THEN 'Locked account / role' ELSE 'User' END FROM mysql.user ORDER BY User, Host".to_owned(),
        EngineKind::Mariadb => "SELECT User, CASE WHEN is_role = 'Y' THEN NULL ELSE Host END, CASE WHEN is_role = 'Y' THEN 'Role' ELSE 'User' END FROM mysql.user ORDER BY User, Host".to_owned(),
        EngineKind::Sqlite => unreachable!(),
    })
}

/// Columns: directly assigned role, member. Role activation and inheritance
/// vary by engine; a membership is not a claim of effective access.
pub fn memberships_query(engine: EngineKind) -> DbResult<String> {
    ensure_supported(engine)?;
    Ok(match engine {
        EngineKind::Postgres => "SELECT role.rolname::text, member.rolname::text FROM pg_auth_members m JOIN pg_roles role ON role.oid = m.roleid JOIN pg_roles member ON member.oid = m.member ORDER BY 1, 2".to_owned(),
        EngineKind::Mysql => "SELECT CONCAT(FROM_USER, '@', FROM_HOST), CONCAT(TO_USER, '@', TO_HOST) FROM mysql.role_edges ORDER BY 1, 2".to_owned(),
        EngineKind::Mariadb => "SELECT Role, CASE WHEN Host = '' THEN User ELSE CONCAT(User, '@', Host) END FROM mysql.roles_mapping ORDER BY 1, 2".to_owned(),
        EngineKind::Sqlite => unreachable!(),
    })
}

/// Direct grants for a scope, including PUBLIC and other principals. Ownership
/// and inherited permissions are deliberately not flattened into these rows.
/// Columns: grantee, privilege, grantable, grantor/source.
pub fn grants_query(engine: EngineKind, scope: &PermissionScope) -> DbResult<String> {
    ensure_supported(engine)?;
    // Validate the same names and scope used for mutations.
    scope.render(engine)?;
    if engine == EngineKind::Postgres {
        return postgres_grants(scope);
    }
    if engine == EngineKind::Mariadb {
        return super::mariadb::grants(scope);
    }

    let database = literal(engine, &scope.database)?;
    let object = literal(engine, &scope.object)?;
    let column = literal(engine, &scope.column)?;
    let (table, condition) = match scope.kind {
        ScopeKind::Server => ("USER_PRIVILEGES", "1 = 1".to_owned()),
        ScopeKind::Database => (
            "SCHEMA_PRIVILEGES",
            format!(
                "TABLE_SCHEMA IN ({database}, {})",
                literal(engine, &database_grant_pattern(&scope.database))?
            ),
        ),
        ScopeKind::Table => (
            "TABLE_PRIVILEGES",
            format!("TABLE_SCHEMA = {database} AND TABLE_NAME = {object}"),
        ),
        ScopeKind::Column => (
            "COLUMN_PRIVILEGES",
            format!(
                "TABLE_SCHEMA = {database} AND TABLE_NAME = {object} AND COLUMN_NAME = {column}"
            ),
        ),
        ScopeKind::Function | ScopeKind::Procedure => {
            // Neither engine exposes ROUTINE_PRIVILEGES in information_schema.
            return super::mariadb::routine_grants(engine, scope);
        }
        _ => {
            return Err(DatabaseError::invalid_request(
                "Unsupported permission scope",
            ));
        }
    };
    Ok(format!(
        "SELECT GRANTEE, PRIVILEGE_TYPE, IS_GRANTABLE, 'Direct' FROM information_schema.{table} WHERE {condition} ORDER BY 1, 2"
    ))
}

fn postgres_grants(scope: &PermissionScope) -> DbResult<String> {
    let engine = EngineKind::Postgres;
    let database = literal(engine, &scope.database)?;
    let namespace = literal(engine, &scope.namespace)?;
    let object = literal(engine, &scope.object)?;
    let column = literal(engine, &scope.column)?;
    let (source, acl, condition) = match scope.kind {
        ScopeKind::Database => (
            "pg_database o".to_owned(),
            "COALESCE(o.datacl, acldefault('d', o.datdba))".to_owned(),
            format!("o.datname = {database}"),
        ),
        ScopeKind::Schema => (
            "pg_namespace o".to_owned(),
            "COALESCE(o.nspacl, acldefault('n', o.nspowner))".to_owned(),
            format!("o.nspname = {namespace}"),
        ),
        ScopeKind::Table | ScopeKind::Sequence => {
            let kind = if scope.kind == ScopeKind::Sequence { "S" } else { "r" };
            let relkind = if scope.kind == ScopeKind::Sequence { "o.relkind = 'S'" } else { "o.relkind IN ('r', 'p', 'v', 'm', 'f')" };
            (
                "pg_class o JOIN pg_namespace n ON n.oid = o.relnamespace".to_owned(),
                format!("COALESCE(o.relacl, acldefault('{kind}', o.relowner))"),
                format!("n.nspname = {namespace} AND o.relname = {object} AND {relkind}"),
            )
        }
        ScopeKind::Column => (
            "pg_attribute o JOIN pg_class c ON c.oid = o.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace".to_owned(),
            "o.attacl".to_owned(),
            format!("n.nspname = {namespace} AND c.relname = {object} AND o.attname = {column} AND NOT o.attisdropped"),
        ),
        ScopeKind::Function | ScopeKind::Procedure => {
            let kind = if scope.kind == ScopeKind::Procedure { "p" } else { "f" };
            (
                "pg_proc o JOIN pg_namespace n ON n.oid = o.pronamespace".to_owned(),
                "COALESCE(o.proacl, acldefault('f', o.proowner))".to_owned(),
                format!("n.nspname = {namespace} AND o.proname = {object} AND o.prokind = '{kind}'"),
            )
        }
        _ => return Err(DatabaseError::invalid_request("PostgreSQL has no server-wide object grants")),
    };
    Ok(format!(
        "SELECT CASE WHEN a.grantee = 0 THEN 'PUBLIC' ELSE pg_get_userbyid(a.grantee)::text END, a.privilege_type::text, CASE WHEN a.is_grantable THEN 'YES' ELSE 'NO' END::text, pg_get_userbyid(a.grantor)::text FROM {source} CROSS JOIN LATERAL aclexplode({acl}) a WHERE {condition} ORDER BY 1, 2"
    ))
}

pub(super) fn routine_count_query(scope: &PermissionScope) -> DbResult<String> {
    let namespace = literal(EngineKind::Postgres, &scope.namespace)?;
    let object = literal(EngineKind::Postgres, &scope.object)?;
    let kind = if scope.kind == ScopeKind::Procedure {
        "p"
    } else {
        "f"
    };
    Ok(format!(
        "SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = {namespace} AND p.proname = {object} AND p.prokind = '{kind}'"
    ))
}
