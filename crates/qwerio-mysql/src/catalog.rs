//! Metadata from INFORMATION_SCHEMA; identifiers are never interpolated as SQL values.

use crate::error::map_error;
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Row, from_row_opt};
use qwerio_core::catalog::{ColumnInfo, ForeignKeyInfo, KeyInfo, ObjectDetails};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::{
    CatalogEntry, CatalogEntryKind, CatalogPath, EntryAccess, ObjectKind, ObjectRef,
};
use std::collections::BTreeMap;

fn metadata<T: mysql_async::prelude::FromRow>(row: Row) -> DbResult<T> {
    from_row_opt(row)
        .map_err(|error| DatabaseError::internal(format!("Invalid catalog result: {error}")))
}

pub(crate) async fn list(client: &mut Conn, path: &CatalogPath) -> DbResult<Vec<CatalogEntry>> {
    if path.namespace.is_some() {
        return Err(DatabaseError::invalid_request(
            "MySQL databases have no separate schema level",
        ));
    }
    let Some(database) = &path.database else {
        let names: Vec<String> = client.query("SHOW DATABASES").await.map_err(map_error)?;
        return Ok(names
            .into_iter()
            .map(|name| CatalogEntry {
                name,
                kind: CatalogEntryKind::Database,
                access: EntryAccess::Accessible,
                comment: None,
            })
            .collect());
    };
    let rows: Vec<Row> = client.exec(
        "SELECT TABLE_NAME, TABLE_TYPE, TABLE_COMMENT FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
        (database,),
    ).await.map_err(map_error)?;
    rows.into_iter()
        .map(|row| {
            let (name, kind, comment): (String, String, String) = metadata(row)?;
            Ok(CatalogEntry {
                name,
                kind: CatalogEntryKind::Object(object_kind(&kind)),
                access: EntryAccess::Accessible,
                comment: non_empty(comment),
            })
        })
        .collect()
}

fn object_kind(kind: &str) -> ObjectKind {
    if kind == "VIEW" || kind == "SYSTEM VIEW" {
        ObjectKind::View
    } else {
        ObjectKind::Table
    }
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

pub(crate) fn database(object: &ObjectRef) -> DbResult<&str> {
    if object.namespace.is_some() {
        return Err(DatabaseError::invalid_request(
            "MySQL databases have no separate schema level",
        ));
    }
    object
        .database
        .as_deref()
        .ok_or_else(|| DatabaseError::invalid_request("Choose a database"))
}

pub(crate) async fn describe(client: &mut Conn, object: &ObjectRef) -> DbResult<ObjectDetails> {
    let database = database(object)?;
    let table: Option<(String, String)> = client.exec_first(
        "SELECT TABLE_TYPE, TABLE_COMMENT FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
        (database, &object.name),
    ).await.map_err(map_error)?;
    let Some((kind, comment)) = table else {
        return Err(DatabaseError::new(
            ErrorKind::UndefinedObject,
            format!(
                "Object \"{}\" does not exist or is not accessible",
                object.name
            ),
        ));
    };
    if object_kind(&kind) != object.kind {
        return Err(DatabaseError::new(
            ErrorKind::UndefinedObject,
            "The object's kind changed; refresh and try again",
        ));
    }
    let rows: Vec<Row> = client.exec(
        "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, ORDINAL_POSITION, COLUMN_COMMENT, EXTRA, GENERATION_EXPRESSION \
         FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
        (database, &object.name),
    ).await.map_err(map_error)?;
    let columns = rows
        .into_iter()
        .map(|row| {
            let (name, type_name, nullable, default, ordinal, comment, extra, expression): (
                String,
                String,
                String,
                Option<String>,
                u64,
                String,
                String,
                Option<String>,
            ) = metadata(row)?;
            Ok(ColumnInfo {
                name,
                type_name,
                cast_type: None,
                nullable: nullable == "YES",
                default,
                ordinal: ordinal as usize,
                comment: non_empty(comment),
                decodable: true,
                generated: expression.is_some_and(|text| !text.is_empty())
                    || extra.contains("VIRTUAL GENERATED")
                    || extra.contains("STORED GENERATED"),
            })
        })
        .collect::<DbResult<Vec<_>>>()?;
    let keys = unique_keys(client, database, &object.name, &columns).await?;
    let foreign_keys = relationships(client, object.connection, &CatalogPath::database(database))
        .await?
        .into_iter()
        .filter(|key| key.table.same_location_and_name(object))
        .collect();
    let referencing_keys = referencing_keys(client, object).await?;
    Ok(ObjectDetails {
        object: object.clone(),
        columns,
        primary_key: keys
            .iter()
            .find(|key| key.name.as_deref() == Some("PRIMARY"))
            .cloned(),
        unique_keys: keys
            .into_iter()
            .filter(|key| key.name.as_deref() != Some("PRIMARY"))
            .collect(),
        row_locator: None,
        foreign_keys,
        referencing_keys,
        comment: non_empty(comment),
    })
}

async fn unique_keys(
    client: &mut Conn,
    database: &str,
    table: &str,
    columns: &[ColumnInfo],
) -> DbResult<Vec<KeyInfo>> {
    let rows: Vec<Row> = client.exec(
        "SELECT INDEX_NAME, COLUMN_NAME FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND NON_UNIQUE = 0 ORDER BY INDEX_NAME, SEQ_IN_INDEX",
        (database, table),
    ).await.map_err(map_error)?;
    let mut keys: BTreeMap<String, Vec<Option<String>>> = BTreeMap::new();
    for row in rows {
        let (name, column): (String, Option<String>) = metadata(row)?;
        keys.entry(name).or_default().push(column);
    }
    Ok(keys
        .into_iter()
        .filter_map(|(name, names)| {
            let names = names.into_iter().collect::<Option<Vec<_>>>()?;
            let allows_null = names.iter().any(|name| {
                columns
                    .iter()
                    .find(|column| &column.name == name)
                    .is_none_or(|column| column.nullable)
            });
            Some(KeyInfo {
                name: Some(name),
                columns: names,
                allows_null,
            })
        })
        .collect())
}

const FOREIGN_KEYS: &str = "SELECT CONSTRAINT_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE";

pub(crate) async fn relationships(
    client: &mut Conn,
    connection: ConnectionId,
    path: &CatalogPath,
) -> DbResult<Vec<ForeignKeyInfo>> {
    let database = path
        .database
        .as_deref()
        .ok_or_else(|| DatabaseError::invalid_request("Choose a database to see relationships"))?;
    let rows: Vec<Row> = client.exec(format!("{FOREIGN_KEYS} WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL ORDER BY CONSTRAINT_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION"), (database,)).await.map_err(map_error)?;
    group_foreign_keys(rows, connection)
}

async fn referencing_keys(client: &mut Conn, object: &ObjectRef) -> DbResult<Vec<ForeignKeyInfo>> {
    let rows: Vec<Row> = client.exec(format!("{FOREIGN_KEYS} WHERE REFERENCED_TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME = ? ORDER BY CONSTRAINT_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION"),
        (database(object)?, &object.name)).await.map_err(map_error)?;
    group_foreign_keys(rows, object.connection)
}

fn group_foreign_keys(rows: Vec<Row>, connection: ConnectionId) -> DbResult<Vec<ForeignKeyInfo>> {
    let mut keys: BTreeMap<(String, String, String), ForeignKeyInfo> = BTreeMap::new();
    for row in rows {
        let (database, table, name, column, target_database, target_table, target_column): (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
        ) = metadata(row)?;
        let key = keys
            .entry((database.clone(), table.clone(), name.clone()))
            .or_insert_with(|| ForeignKeyInfo {
                name: Some(name),
                table: ObjectRef {
                    connection,
                    database: Some(database),
                    namespace: None,
                    kind: ObjectKind::Table,
                    name: table,
                },
                columns: Vec::new(),
                referenced_table: ObjectRef {
                    connection,
                    database: Some(target_database),
                    namespace: None,
                    kind: ObjectKind::Table,
                    name: target_table,
                },
                referenced_columns: Vec::new(),
            });
        key.columns.push(column);
        key.referenced_columns.push(target_column);
    }
    Ok(keys.into_values().collect())
}

/// Validates the target storage. Trigger-written tables must also use InnoDB;
/// their storage is a schema-level precondition, not something this check proves.
pub(crate) async fn ensure_transactional(client: &mut Conn, object: &ObjectRef) -> DbResult<()> {
    let engine: Option<String> = client.exec_first("SELECT ENGINE FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?", (database(object)?, &object.name)).await.map_err(map_error)?;
    if engine.as_deref() != Some("InnoDB") {
        return Err(DatabaseError::new(
            ErrorKind::Unsupported,
            "Atomic row editing requires an InnoDB table",
        ));
    }
    Ok(())
}
