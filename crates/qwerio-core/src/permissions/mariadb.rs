//! MariaDB's information_schema omits some role grants. Read only the
//! explicit privilege columns; authentication columns are never selected.

use super::render::literal;
use super::*;

const GLOBAL_PRIVILEGES: &[(&str, &str)] = &[
    ("SELECT", "Select_priv"),
    ("INSERT", "Insert_priv"),
    ("UPDATE", "Update_priv"),
    ("DELETE", "Delete_priv"),
    ("CREATE", "Create_priv"),
    ("ALTER", "Alter_priv"),
    ("DROP", "Drop_priv"),
    ("INDEX", "Index_priv"),
    ("EXECUTE", "Execute_priv"),
    ("CREATE USER", "Create_user_priv"),
    ("SHOW DATABASES", "Show_db_priv"),
];

const DATABASE_PRIVILEGES: &[(&str, &str)] = &[
    ("SELECT", "Select_priv"),
    ("INSERT", "Insert_priv"),
    ("UPDATE", "Update_priv"),
    ("DELETE", "Delete_priv"),
    ("CREATE", "Create_priv"),
    ("ALTER", "Alter_priv"),
    ("DROP", "Drop_priv"),
    ("INDEX", "Index_priv"),
    ("REFERENCES", "References_priv"),
    ("TRIGGER", "Trigger_priv"),
    ("EXECUTE", "Execute_priv"),
    ("CREATE VIEW", "Create_view_priv"),
    ("SHOW VIEW", "Show_view_priv"),
    ("CREATE ROUTINE", "Create_routine_priv"),
    ("ALTER ROUTINE", "Alter_routine_priv"),
    ("CREATE TEMPORARY TABLES", "Create_tmp_table_priv"),
    ("LOCK TABLES", "Lock_tables_priv"),
    ("EVENT", "Event_priv"),
];

const TABLE_PRIVILEGES: &[(&str, &str)] = &[
    ("SELECT", "Select"),
    ("INSERT", "Insert"),
    ("UPDATE", "Update"),
    ("DELETE", "Delete"),
    ("CREATE", "Create"),
    ("ALTER", "Alter"),
    ("DROP", "Drop"),
    ("INDEX", "Index"),
    ("REFERENCES", "References"),
    ("TRIGGER", "Trigger"),
    ("CREATE VIEW", "Create View"),
    ("SHOW VIEW", "Show view"),
];

const GRANTEE: &str = "CASE WHEN Host = '' THEN User ELSE CONCAT(User, '@', Host) END";

pub(super) fn grants(scope: &PermissionScope) -> DbResult<String> {
    let database = literal(EngineKind::Mariadb, &scope.database)?;
    let pattern = literal(
        EngineKind::Mariadb,
        &database_grant_pattern(&scope.database),
    )?;
    let object = literal(EngineKind::Mariadb, &scope.object)?;
    match scope.kind {
        ScopeKind::Server => Ok(column_grants("mysql.user", "1 = 1", GLOBAL_PRIVILEGES)),
        ScopeKind::Database => Ok(column_grants(
            "mysql.db",
            &format!("Db IN ({database}, {pattern})"),
            DATABASE_PRIVILEGES,
        )),
        ScopeKind::Table => Ok(set_grants(
            "mysql.tables_priv",
            &format!("Db = {database} AND Table_name = {object}"),
            "Table_priv",
            TABLE_PRIVILEGES,
        )),
        ScopeKind::Column => {
            let column = literal(EngineKind::Mariadb, &scope.column)?;
            let privileges = &[
                ("SELECT", "Select"),
                ("INSERT", "Insert"),
                ("UPDATE", "Update"),
                ("REFERENCES", "References"),
            ];
            let queries: Vec<_> = privileges.iter().map(|(label, member)| format!(
                "SELECT CASE WHEN c.Host = '' THEN c.User ELSE CONCAT(c.User, '@', c.Host) END, '{label}', CASE WHEN FIND_IN_SET('Grant', t.Table_priv) > 0 THEN 'YES' ELSE 'NO' END, 'Direct' FROM mysql.columns_priv c LEFT JOIN mysql.tables_priv t ON t.Host = c.Host AND t.User = c.User AND t.Db = c.Db AND t.Table_name = c.Table_name WHERE c.Db = {database} AND c.Table_name = {object} AND c.Column_name = {column} AND FIND_IN_SET('{member}', c.Column_priv) > 0"
            )).collect();
            Ok(format!("{} ORDER BY 1, 2", queries.join(" UNION ALL ")))
        }
        ScopeKind::Function | ScopeKind::Procedure => routine_grants(EngineKind::Mariadb, scope),
        _ => Err(DatabaseError::invalid_request(
            "Unsupported MariaDB permission scope",
        )),
    }
}

fn column_grants(table: &str, condition: &str, privileges: &[(&str, &str)]) -> String {
    let queries: Vec<_> = privileges.iter().map(|(label, column)| format!(
        "SELECT {GRANTEE}, '{label}', CASE WHEN Grant_priv = 'Y' THEN 'YES' ELSE 'NO' END, 'Direct' FROM {table} WHERE {condition} AND {column} = 'Y'"
    )).collect();
    format!("{} ORDER BY 1, 2", queries.join(" UNION ALL "))
}

fn set_grants(table: &str, condition: &str, column: &str, privileges: &[(&str, &str)]) -> String {
    let queries: Vec<_> = privileges.iter().map(|(label, member)| format!(
        "SELECT {GRANTEE}, '{label}', CASE WHEN FIND_IN_SET('Grant', {column}) > 0 THEN 'YES' ELSE 'NO' END, 'Direct' FROM {table} WHERE {condition} AND FIND_IN_SET('{member}', {column}) > 0"
    )).collect();
    format!("{} ORDER BY 1, 2", queries.join(" UNION ALL "))
}

pub(super) fn routine_grants(engine: EngineKind, scope: &PermissionScope) -> DbResult<String> {
    let database = literal(engine, &scope.database)?;
    let object = literal(engine, &scope.object)?;
    let kind = if scope.kind == ScopeKind::Function {
        "FUNCTION"
    } else {
        "PROCEDURE"
    };
    Ok(set_grants(
        "mysql.procs_priv",
        &format!("Db = {database} AND Routine_name = {object} AND Routine_type = '{kind}'"),
        "Proc_priv",
        &[("EXECUTE", "Execute"), ("ALTER ROUTINE", "Alter Routine")],
    ))
}
