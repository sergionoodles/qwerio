//! Catalog discovery from `pg_catalog`, exposing privilege limitations rather
//! than hiding inaccessible objects. Names are always bound as parameters.

use qwerio_core::catalog::{ColumnInfo, ForeignKeyInfo, KeyInfo, ObjectDetails};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::{
    CatalogEntry, CatalogEntryKind, CatalogPath, EntryAccess, ObjectKind, ObjectRef,
};
use tokio_postgres::Client;

use crate::error::map_error;

/// Base types (by `format_type` name) that `decode.rs` decodes natively.
const DECODED_TYPES: [&str; 28] = [
    "boolean",
    "smallint",
    "integer",
    "bigint",
    "oid",
    "real",
    "double precision",
    "numeric",
    "text",
    "character varying",
    "character",
    "name",
    "\"char\"",
    "bytea",
    "json",
    "jsonb",
    "uuid",
    "date",
    "time without time zone",
    "time with time zone",
    "timestamp without time zone",
    "timestamp with time zone",
    "interval",
    "inet",
    "cidr",
    "macaddr",
    "xml",
    "unknown",
];

fn is_decoded_type(name: &str) -> bool {
    DECODED_TYPES.contains(&name)
}

pub(crate) async fn list(
    client: &Client,
    session_database: &str,
    path: &CatalogPath,
) -> DbResult<Vec<CatalogEntry>> {
    match (&path.database, &path.namespace) {
        (None, None) => list_databases(client).await,
        (Some(database), None) => {
            ensure_session_database(session_database, database)?;
            list_namespaces(client).await
        }
        (database, Some(namespace)) => {
            if let Some(database) = database {
                ensure_session_database(session_database, database)?;
            }

            list_objects(client, namespace).await
        }
    }
}

fn ensure_session_database(session_database: &str, requested: &str) -> DbResult<()> {
    if session_database == requested {
        return Ok(());
    }

    Err(DatabaseError::invalid_request(format!(
        "This session is connected to \"{session_database}\", not \"{requested}\""
    )))
}

async fn list_databases(client: &Client) -> DbResult<Vec<CatalogEntry>> {
    let rows = client
        .query(
            "SELECT datname, has_database_privilege(oid, 'CONNECT'), \
                    shobj_description(oid, 'pg_database') \
             FROM pg_catalog.pg_database \
             WHERE NOT datistemplate AND datallowconn \
             ORDER BY datname",
            &[],
        )
        .await
        .map_err(|error| map_error(error, None))?;

    rows.iter()
        .map(|row| {
            Ok(CatalogEntry {
                kind: CatalogEntryKind::Database,
                name: row.try_get(0).map_err(|error| map_error(error, None))?,
                access: access(row.try_get(1).map_err(|error| map_error(error, None))?),
                comment: row.try_get(2).map_err(|error| map_error(error, None))?,
            })
        })
        .collect()
}

async fn list_namespaces(client: &Client) -> DbResult<Vec<CatalogEntry>> {
    let rows = client
        .query(
            "SELECT nspname, has_schema_privilege(oid, 'USAGE'), \
                    (nspname LIKE 'pg\\_%' OR nspname = 'information_schema') AS is_system, \
                    obj_description(oid, 'pg_namespace') \
             FROM pg_catalog.pg_namespace \
             WHERE nspname NOT LIKE 'pg\\_toast%' AND nspname NOT LIKE 'pg\\_temp\\_%' \
             ORDER BY 3, nspname",
            &[],
        )
        .await
        .map_err(|error| map_error(error, None))?;

    rows.iter()
        .map(|row| {
            let get_error = |error| map_error(error, None);

            Ok(CatalogEntry {
                kind: CatalogEntryKind::Namespace {
                    is_system: row.try_get(2).map_err(get_error)?,
                },
                name: row.try_get(0).map_err(get_error)?,
                access: access(row.try_get(1).map_err(get_error)?),
                comment: row.try_get(3).map_err(get_error)?,
            })
        })
        .collect()
}

async fn list_objects(client: &Client, namespace: &str) -> DbResult<Vec<CatalogEntry>> {
    let rows = client
        .query(
            "SELECT c.relname, c.relkind::text, has_table_privilege(c.oid, 'SELECT'), \
                    obj_description(c.oid, 'pg_class') \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm', 'f') AND NOT c.relispartition \
             ORDER BY c.relname",
            &[&namespace],
        )
        .await
        .map_err(|error| map_error(error, None))?;

    rows.iter()
        .map(|row| {
            let get_error = |error| map_error(error, None);
            let relkind: String = row.try_get(1).map_err(get_error)?;

            Ok(CatalogEntry {
                kind: CatalogEntryKind::Object(object_kind(&relkind)),
                name: row.try_get(0).map_err(get_error)?,
                access: access(row.try_get(2).map_err(get_error)?),
                comment: row.try_get(3).map_err(get_error)?,
            })
        })
        .collect()
}

fn access(has_privilege: bool) -> EntryAccess {
    if has_privilege {
        EntryAccess::Accessible
    } else {
        EntryAccess::NoPrivilege
    }
}

fn object_kind(relkind: &str) -> ObjectKind {
    match relkind {
        "v" => ObjectKind::View,
        "m" => ObjectKind::MaterializedView,
        "f" => ObjectKind::ForeignTable,
        _ => ObjectKind::Table,
    }
}

pub(crate) async fn describe(client: &Client, object: &ObjectRef) -> DbResult<ObjectDetails> {
    let namespace = object
        .namespace
        .as_deref()
        .ok_or_else(|| DatabaseError::invalid_request("PostgreSQL objects need a schema"))?;

    let found = client
        .query_opt(
            "SELECT c.oid, c.relkind::text, obj_description(c.oid, 'pg_class') \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r', 'p', 'v', 'm', 'f')",
            &[&namespace, &object.name],
        )
        .await
        .map_err(|error| map_error(error, None))?
        .ok_or_else(|| {
            DatabaseError::new(
                ErrorKind::UndefinedObject,
                format!("No table or view named \"{}\"", object.qualified_name()),
            )
        })?;

    let get_error = |error| map_error(error, None);
    let oid: u32 = found.try_get(0).map_err(get_error)?;
    let relkind: String = found.try_get(1).map_err(get_error)?;
    let comment: Option<String> = found.try_get(2).map_err(get_error)?;

    let resolved = ObjectRef {
        kind: object_kind(&relkind),
        ..object.clone()
    };

    let columns = columns(client, oid).await?;
    let (primary_key, unique_keys) = keys(client, oid, &columns).await?;
    let foreign_keys = foreign_keys(client, oid, &resolved).await?;
    let referencing_keys = foreign_keys_referencing(client, oid, &resolved).await?;

    Ok(ObjectDetails {
        object: resolved,
        columns,
        primary_key,
        unique_keys,
        row_locator: None,
        foreign_keys,
        referencing_keys,
        comment,
    })
}

async fn columns(client: &Client, oid: u32) -> DbResult<Vec<ColumnInfo>> {
    let rows = client
        .query(
            "SELECT a.attname, \
                    format_type(a.atttypid, a.atttypmod), \
                    format_type(a.atttypid, NULL), \
                    NOT a.attnotnull, \
                    pg_get_expr(d.adbin, d.adrelid), \
                    col_description(a.attrelid, a.attnum), \
                    format_type(base.oid, NULL), \
                    base.typtype = 'e', \
                    format_type(element.oid, NULL), \
                    element.typtype = 'e' \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_type t ON t.oid = a.atttypid \
             JOIN pg_catalog.pg_type base ON base.oid = COALESCE(NULLIF(t.typbasetype, 0), t.oid) \
             LEFT JOIN pg_catalog.pg_type element \
                    ON base.typcategory = 'A' AND element.oid = base.typelem \
             LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            &[&oid],
        )
        .await
        .map_err(|error| map_error(error, None))?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let get_error = |error| map_error(error, None);
            let base_type: String = row.try_get(6).map_err(get_error)?;
            let is_enum: bool = row.try_get(7).map_err(get_error)?;
            let element_type: Option<String> = row.try_get(8).map_err(get_error)?;
            let is_enum_element: Option<bool> = row.try_get(9).map_err(get_error)?;

            let decodable = is_enum
                || is_decoded_type(&base_type)
                || is_enum_element == Some(true)
                || element_type.as_deref().is_some_and(is_decoded_type);

            Ok(ColumnInfo {
                name: row.try_get(0).map_err(get_error)?,
                type_name: row.try_get(1).map_err(get_error)?,
                cast_type: Some(row.try_get(2).map_err(get_error)?),
                nullable: row.try_get(3).map_err(get_error)?,
                default: row.try_get(4).map_err(get_error)?,
                ordinal: index + 1,
                comment: row.try_get(5).map_err(get_error)?,
                decodable,
            })
        })
        .collect()
}

/// The primary key and other unique, non-partial, non-expression indexes.
async fn keys(
    client: &Client,
    oid: u32,
    columns: &[ColumnInfo],
) -> DbResult<(Option<KeyInfo>, Vec<KeyInfo>)> {
    let rows = client
        .query(
            "SELECT i.indisprimary, ic.relname, \
                    ARRAY(SELECT a.attname::text \
                          FROM unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, position) \
                          JOIN pg_catalog.pg_attribute a \
                            ON a.attrelid = i.indrelid AND a.attnum = k.attnum \
                          WHERE k.position <= i.indnkeyatts \
                          ORDER BY k.position) \
             FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid \
             WHERE i.indrelid = $1 AND i.indisunique AND i.indisvalid \
               AND i.indpred IS NULL AND i.indexprs IS NULL \
             ORDER BY NOT i.indisprimary, ic.relname",
            &[&oid],
        )
        .await
        .map_err(|error| map_error(error, None))?;

    let mut primary_key = None;
    let mut unique_keys = Vec::new();

    for row in &rows {
        let get_error = |error| map_error(error, None);
        let is_primary: bool = row.try_get(0).map_err(get_error)?;
        let key_columns: Vec<String> = row.try_get(2).map_err(get_error)?;
        let allows_null = key_columns.iter().any(|name| {
            columns
                .iter()
                .any(|column| &column.name == name && column.nullable)
        });

        let key = KeyInfo {
            name: Some(row.try_get(1).map_err(get_error)?),
            columns: key_columns,
            allows_null,
        };

        if is_primary {
            primary_key = Some(key);
        } else {
            unique_keys.push(key);
        }
    }

    Ok((primary_key, unique_keys))
}

async fn foreign_keys(
    client: &Client,
    oid: u32,
    object: &ObjectRef,
) -> DbResult<Vec<ForeignKeyInfo>> {
    let rows = client
        .query(
            "SELECT con.conname, rn.nspname, rc.relname, rc.relkind::text, \
                    ARRAY(SELECT a.attname::text \
                          FROM unnest(con.conkey) WITH ORDINALITY AS k(attnum, position) \
                          JOIN pg_catalog.pg_attribute a \
                            ON a.attrelid = con.conrelid AND a.attnum = k.attnum \
                          ORDER BY k.position), \
                    ARRAY(SELECT a.attname::text \
                          FROM unnest(con.confkey) WITH ORDINALITY AS k(attnum, position) \
                          JOIN pg_catalog.pg_attribute a \
                            ON a.attrelid = con.confrelid AND a.attnum = k.attnum \
                          ORDER BY k.position) \
             FROM pg_catalog.pg_constraint con \
             JOIN pg_catalog.pg_class rc ON rc.oid = con.confrelid \
             JOIN pg_catalog.pg_namespace rn ON rn.oid = rc.relnamespace \
             WHERE con.conrelid = $1 AND con.contype = 'f' \
             ORDER BY con.conname",
            &[&oid],
        )
        .await
        .map_err(|error| map_error(error, None))?;

    rows.iter()
        .map(|row| {
            let get_error = |error| map_error(error, None);
            let relkind: String = row.try_get(3).map_err(get_error)?;

            Ok(ForeignKeyInfo {
                name: row.try_get(0).map_err(get_error)?,
                table: object.clone(),
                columns: row.try_get(4).map_err(get_error)?,
                referenced_table: ObjectRef {
                    connection: object.connection,
                    database: object.database.clone(),
                    namespace: Some(row.try_get(1).map_err(get_error)?),
                    kind: object_kind(&relkind),
                    name: row.try_get(2).map_err(get_error)?,
                },
                referenced_columns: row.try_get(5).map_err(get_error)?,
            })
        })
        .collect()
}

/// Every foreign key declared on a relation in one schema, including keys
/// that reference relations in other schemas.
pub(crate) async fn relationships(
    client: &Client,
    connection: ConnectionId,
    session_database: &str,
    path: &CatalogPath,
) -> DbResult<Vec<ForeignKeyInfo>> {
    let namespace = path.namespace.as_deref().ok_or_else(|| {
        DatabaseError::invalid_request("PostgreSQL relationships are listed per schema")
    })?;

    if let Some(database) = &path.database {
        ensure_session_database(session_database, database)?;
    }

    let rows = client
        .query(
            "SELECT con.conname, sc.relname, sc.relkind::text, \
                    rn.nspname, rc.relname, rc.relkind::text, \
                    ARRAY(SELECT a.attname::text \
                          FROM unnest(con.conkey) WITH ORDINALITY AS k(attnum, position) \
                          JOIN pg_catalog.pg_attribute a \
                            ON a.attrelid = con.conrelid AND a.attnum = k.attnum \
                          ORDER BY k.position), \
                    ARRAY(SELECT a.attname::text \
                          FROM unnest(con.confkey) WITH ORDINALITY AS k(attnum, position) \
                          JOIN pg_catalog.pg_attribute a \
                            ON a.attrelid = con.confrelid AND a.attnum = k.attnum \
                          ORDER BY k.position) \
             FROM pg_catalog.pg_constraint con \
             JOIN pg_catalog.pg_class sc ON sc.oid = con.conrelid \
             JOIN pg_catalog.pg_namespace sn ON sn.oid = sc.relnamespace \
             JOIN pg_catalog.pg_class rc ON rc.oid = con.confrelid \
             JOIN pg_catalog.pg_namespace rn ON rn.oid = rc.relnamespace \
             WHERE sn.nspname = $1 AND con.contype = 'f' AND NOT sc.relispartition \
             ORDER BY sc.relname, con.conname",
            &[&namespace],
        )
        .await
        .map_err(|error| map_error(error, None))?;

    let database = path.database.clone();

    rows.iter()
        .map(|row| {
            let get_error = |error| map_error(error, None);
            let table_kind: String = row.try_get(2).map_err(get_error)?;
            let referenced_kind: String = row.try_get(5).map_err(get_error)?;

            Ok(ForeignKeyInfo {
                name: row.try_get(0).map_err(get_error)?,
                table: ObjectRef {
                    connection,
                    database: database.clone(),
                    namespace: Some(namespace.to_owned()),
                    kind: object_kind(&table_kind),
                    name: row.try_get(1).map_err(get_error)?,
                },
                columns: row.try_get(6).map_err(get_error)?,
                referenced_table: ObjectRef {
                    connection,
                    database: database.clone(),
                    namespace: Some(row.try_get(3).map_err(get_error)?),
                    kind: object_kind(&referenced_kind),
                    name: row.try_get(4).map_err(get_error)?,
                },
                referenced_columns: row.try_get(7).map_err(get_error)?,
            })
        })
        .collect()
}

async fn foreign_keys_referencing(
    client: &Client,
    oid: u32,
    object: &ObjectRef,
) -> DbResult<Vec<ForeignKeyInfo>> {
    let rows = client
        .query(
            "SELECT con.conname, sn.nspname, sc.relname, sc.relkind::text, \
                    ARRAY(SELECT a.attname::text \
                          FROM unnest(con.conkey) WITH ORDINALITY AS k(attnum, position) \
                          JOIN pg_catalog.pg_attribute a \
                            ON a.attrelid = con.conrelid AND a.attnum = k.attnum \
                          ORDER BY k.position), \
                    ARRAY(SELECT a.attname::text \
                          FROM unnest(con.confkey) WITH ORDINALITY AS k(attnum, position) \
                          JOIN pg_catalog.pg_attribute a \
                            ON a.attrelid = con.confrelid AND a.attnum = k.attnum \
                          ORDER BY k.position) \
             FROM pg_catalog.pg_constraint con \
             JOIN pg_catalog.pg_class sc ON sc.oid = con.conrelid \
             JOIN pg_catalog.pg_namespace sn ON sn.oid = sc.relnamespace \
             WHERE con.confrelid = $1 AND con.contype = 'f' \
             ORDER BY sn.nspname, sc.relname, con.conname",
            &[&oid],
        )
        .await
        .map_err(|error| map_error(error, None))?;

    rows.iter()
        .map(|row| {
            let get_error = |error| map_error(error, None);
            let relkind: String = row.try_get(3).map_err(get_error)?;

            Ok(ForeignKeyInfo {
                name: row.try_get(0).map_err(get_error)?,
                table: ObjectRef {
                    connection: object.connection,
                    database: object.database.clone(),
                    namespace: Some(row.try_get(1).map_err(get_error)?),
                    kind: object_kind(&relkind),
                    name: row.try_get(2).map_err(get_error)?,
                },
                columns: row.try_get(4).map_err(get_error)?,
                referenced_table: object.clone(),
                referenced_columns: row.try_get(5).map_err(get_error)?,
            })
        })
        .collect()
}
