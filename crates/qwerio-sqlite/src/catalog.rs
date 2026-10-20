//! Catalog discovery through pragma table-valued functions. Object names are
//! always bound as parameters, never interpolated.

use qwerio_core::catalog::{ColumnInfo, ForeignKeyInfo, KeyInfo, ObjectDetails, RowLocator};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::{
    CatalogEntry, CatalogEntryKind, CatalogPath, EntryAccess, ObjectKind, ObjectRef,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::map_error;

/// Names SQLite reserves for the implicit rowid, in order of preference.
const ROWID_ALIASES: [&str; 3] = ["rowid", "_rowid_", "oid"];

pub(crate) fn list(connection: &Connection, path: &CatalogPath) -> DbResult<Vec<CatalogEntry>> {
    if path.database.is_some() || path.namespace.is_some() {
        return Err(DatabaseError::new(
            ErrorKind::NotFound,
            "SQLite databases have no nested databases or schemas",
        ));
    }

    let mut statement = connection
        .prepare(
            "SELECT name, type FROM main.sqlite_schema \
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             ORDER BY name COLLATE NOCASE, name",
        )
        .map_err(map_error)?;

    let entries = statement
        .query_map([], |row| {
            let name: String = row.get(0)?;
            let kind: String = row.get(1)?;
            Ok((name, kind))
        })
        .map_err(map_error)?
        .map(|row| {
            row.map(|(name, kind)| CatalogEntry {
                kind: CatalogEntryKind::Object(object_kind(&kind)),
                name,
                access: EntryAccess::Accessible,
                comment: None,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;

    Ok(entries)
}

fn object_kind(schema_type: &str) -> ObjectKind {
    if schema_type == "view" {
        ObjectKind::View
    } else {
        ObjectKind::Table
    }
}

pub(crate) fn describe(connection: &Connection, object: &ObjectRef) -> DbResult<ObjectDetails> {
    let kind = find_object_kind(connection, &object.name)?;
    let resolved = ObjectRef {
        kind,
        database: None,
        namespace: None,
        ..object.clone()
    };

    let ColumnListing {
        columns,
        primary_key_positions,
    } = columns(connection, &object.name, kind)?;
    let is_without_rowid = kind == ObjectKind::Table && is_without_rowid(connection, &object.name)?;
    let primary_key = primary_key(&columns, primary_key_positions, is_without_rowid);

    let row_locator = (kind == ObjectKind::Table && !is_without_rowid)
        .then(|| rowid_locator(&columns))
        .flatten();

    let unique_keys = if kind == ObjectKind::Table {
        unique_keys(connection, &object.name, &columns)?
    } else {
        Vec::new()
    };

    let foreign_keys = if kind == ObjectKind::Table {
        foreign_keys(connection, &resolved)?
    } else {
        Vec::new()
    };

    let referencing_keys = referencing_keys(connection, &resolved)?;

    Ok(ObjectDetails {
        object: resolved,
        columns,
        primary_key,
        unique_keys,
        row_locator,
        foreign_keys,
        referencing_keys,
        comment: None,
    })
}

fn find_object_kind(connection: &Connection, name: &str) -> DbResult<ObjectKind> {
    connection
        .query_row(
            "SELECT type FROM main.sqlite_schema WHERE type IN ('table', 'view') AND name = ?1",
            params![name],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_error)?
        .map(|kind| object_kind(&kind))
        .ok_or_else(|| {
            DatabaseError::new(
                ErrorKind::UndefinedObject,
                format!("No table or view named \"{name}\""),
            )
        })
}

/// `pragma_table_xinfo.hidden` of a hidden virtual-table column.
const HIDDEN_VIRTUAL_TABLE_COLUMN: i64 = 1;

/// `pragma_table_xinfo.hidden` of virtual and stored generated columns.
const GENERATED_COLUMNS: [i64; 2] = [2, 3];

/// Columns in declaration order and the primary key position of each key column.
struct ColumnListing {
    columns: Vec<ColumnInfo>,
    primary_key_positions: Vec<(usize, String)>,
}

fn columns(connection: &Connection, name: &str, kind: ObjectKind) -> DbResult<ColumnListing> {
    let mut statement = connection
        .prepare(
            "SELECT name, type, \"notnull\", dflt_value, pk, hidden \
             FROM pragma_table_xinfo(?1, 'main') ORDER BY cid",
        )
        .map_err(map_error)?;

    let mut rows = statement.query(params![name]).map_err(map_error)?;
    let mut columns = Vec::new();
    let mut primary_key_positions = Vec::new();

    while let Some(row) = rows.next().map_err(map_error)? {
        let hidden: i64 = row.get(5).map_err(map_error)?;

        // Hidden virtual-table columns are not selectable data.
        if hidden == HIDDEN_VIRTUAL_TABLE_COLUMN {
            continue;
        }

        let column_name: String = row.get(0).map_err(map_error)?;
        let type_name: String = row.get(1).map_err(map_error)?;
        let is_not_null: bool = row.get(2).map_err(map_error)?;
        let default: Option<String> = row.get(3).map_err(map_error)?;
        let primary_key_position: i64 = row.get(4).map_err(map_error)?;

        if let Ok(position) = usize::try_from(primary_key_position)
            && position > 0
        {
            primary_key_positions.push((position, column_name.clone()));
        }

        columns.push(ColumnInfo {
            name: column_name,
            type_name,
            cast_type: None,
            // View columns report no constraints; treat them as nullable.
            nullable: kind != ObjectKind::Table || !is_not_null,
            default,
            ordinal: columns.len() + 1,
            comment: None,
            decodable: true,
            generated: GENERATED_COLUMNS.contains(&hidden),
        });
    }

    Ok(ColumnListing {
        columns,
        primary_key_positions,
    })
}

fn primary_key(
    columns: &[ColumnInfo],
    mut positions: Vec<(usize, String)>,
    is_without_rowid: bool,
) -> Option<KeyInfo> {
    if positions.is_empty() {
        return None;
    }

    positions.sort();
    let key_columns: Vec<String> = positions.into_iter().map(|(_, name)| name).collect();

    // Rowid tables accept NULL in primary keys unless the key is the INTEGER
    // PRIMARY KEY rowid alias or its columns are declared NOT NULL.
    let is_rowid_alias = key_columns.len() == 1
        && columns.iter().any(|column| {
            column.name == key_columns[0] && column.type_name.eq_ignore_ascii_case("integer")
        });
    let is_declared_not_null = key_columns.iter().all(|name| {
        columns
            .iter()
            .any(|column| &column.name == name && !column.nullable)
    });

    Some(KeyInfo {
        name: None,
        columns: key_columns,
        allows_null: !(is_without_rowid || is_rowid_alias || is_declared_not_null),
    })
}

fn is_without_rowid(connection: &Connection, name: &str) -> DbResult<bool> {
    connection
        .query_row(
            "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
            params![name],
            |row| row.get::<_, bool>(0),
        )
        .optional()
        .map_err(map_error)
        .map(|value| value.unwrap_or(false))
}

fn rowid_locator(columns: &[ColumnInfo]) -> Option<RowLocator> {
    ROWID_ALIASES
        .iter()
        .find(|alias| {
            !columns
                .iter()
                .any(|column| column.name.eq_ignore_ascii_case(alias))
        })
        .map(|alias| RowLocator {
            expression: (*alias).to_owned(),
            type_name: "INTEGER".to_owned(),
        })
}

/// Full (non-partial) unique indexes over plain columns.
fn unique_keys(
    connection: &Connection,
    name: &str,
    columns: &[ColumnInfo],
) -> DbResult<Vec<KeyInfo>> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM pragma_index_list(?1, 'main') \
             WHERE \"unique\" = 1 AND partial = 0 AND origin <> 'pk' ORDER BY seq",
        )
        .map_err(map_error)?;

    let index_names = statement
        .query_map(params![name], |row| row.get::<_, String>(0))
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;

    let mut keys = Vec::new();

    for index_name in index_names {
        let mut index_statement = connection
            .prepare("SELECT name FROM pragma_index_info(?1, 'main') ORDER BY seqno")
            .map_err(map_error)?;

        let key_columns = index_statement
            .query_map(params![index_name], |row| row.get::<_, Option<String>>(0))
            .map_err(map_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_error)?;

        // Expression indexes have unnamed columns.
        let Some(key_columns) = key_columns.into_iter().collect::<Option<Vec<_>>>() else {
            continue;
        };

        let allows_null = !key_columns.iter().all(|key_column| {
            columns
                .iter()
                .any(|column| &column.name == key_column && !column.nullable)
        });

        keys.push(KeyInfo {
            name: Some(index_name),
            columns: key_columns,
            allows_null,
        });
    }

    Ok(keys)
}

struct ForeignKeyRow {
    table: String,
    id: i64,
    referenced_table: String,
    from: String,
    to: Option<String>,
}

fn foreign_keys(connection: &Connection, object: &ObjectRef) -> DbResult<Vec<ForeignKeyInfo>> {
    let mut statement = connection
        .prepare(
            "SELECT id, \"table\", \"from\", \"to\" FROM pragma_foreign_key_list(?1, 'main') \
             ORDER BY id, seq",
        )
        .map_err(map_error)?;

    let rows = statement
        .query_map(params![object.name], |row| {
            Ok(ForeignKeyRow {
                table: object.name.clone(),
                id: row.get(0)?,
                referenced_table: row.get(1)?,
                from: row.get(2)?,
                to: row.get(3)?,
            })
        })
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;

    group_foreign_keys(connection, object.connection, rows)
}

fn referencing_keys(connection: &Connection, object: &ObjectRef) -> DbResult<Vec<ForeignKeyInfo>> {
    let mut statement = connection
        .prepare(
            "SELECT m.name, f.id, f.\"table\", f.\"from\", f.\"to\" \
             FROM main.sqlite_schema AS m, pragma_foreign_key_list(m.name, 'main') AS f \
             WHERE m.type = 'table' AND f.\"table\" = ?1 COLLATE NOCASE \
             ORDER BY m.name, f.id, f.seq",
        )
        .map_err(map_error)?;

    let rows = statement
        .query_map(params![object.name], |row| {
            Ok(ForeignKeyRow {
                table: row.get(0)?,
                id: row.get(1)?,
                referenced_table: row.get(2)?,
                from: row.get(3)?,
                to: row.get(4)?,
            })
        })
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;

    group_foreign_keys(connection, object.connection, rows)
}

/// Every foreign key in the database; SQLite has only the root container.
pub(crate) fn relationships(
    connection: &Connection,
    connection_id: ConnectionId,
    path: &CatalogPath,
) -> DbResult<Vec<ForeignKeyInfo>> {
    if path.database.is_some() || path.namespace.is_some() {
        return Err(DatabaseError::new(
            ErrorKind::NotFound,
            "SQLite databases have no nested databases or schemas",
        ));
    }

    let mut statement = connection
        .prepare(
            "SELECT m.name, f.id, f.\"table\", f.\"from\", f.\"to\" \
             FROM main.sqlite_schema AS m, pragma_foreign_key_list(m.name, 'main') AS f \
             WHERE m.type = 'table' \
             ORDER BY m.name, f.id, f.seq",
        )
        .map_err(map_error)?;

    let rows = statement
        .query_map([], |row| {
            Ok(ForeignKeyRow {
                table: row.get(0)?,
                id: row.get(1)?,
                referenced_table: row.get(2)?,
                from: row.get(3)?,
                to: row.get(4)?,
            })
        })
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;

    group_foreign_keys(connection, connection_id, rows)
}

/// Groups pragma rows by (table, id) into ordered composite keys. A missing
/// `to` column means the referenced table's primary key.
fn group_foreign_keys(
    connection: &Connection,
    connection_id: ConnectionId,
    rows: Vec<ForeignKeyRow>,
) -> DbResult<Vec<ForeignKeyInfo>> {
    let mut keys: Vec<(String, i64, ForeignKeyInfo, bool)> = Vec::new();

    for row in rows {
        let table_ref = |name: &str| ObjectRef {
            connection: connection_id,
            database: None,
            namespace: None,
            kind: ObjectKind::Table,
            name: name.to_owned(),
        };

        let is_same_key = keys
            .last()
            .is_some_and(|(table, id, _, _)| *table == row.table && *id == row.id);

        if !is_same_key {
            keys.push((
                row.table.clone(),
                row.id,
                ForeignKeyInfo {
                    name: None,
                    table: table_ref(&row.table),
                    columns: Vec::new(),
                    referenced_table: table_ref(&row.referenced_table),
                    referenced_columns: Vec::new(),
                },
                false,
            ));
        }

        if let Some((_, _, key, references_primary_key)) = keys.last_mut() {
            key.columns.push(row.from);

            match row.to {
                Some(to) => key.referenced_columns.push(to),
                None => *references_primary_key = true,
            }
        }
    }

    keys.into_iter()
        .map(|(_, _, mut key, references_primary_key)| {
            if references_primary_key {
                key.referenced_columns =
                    declared_primary_key(connection, &key.referenced_table.name)?;
            }

            canonicalize_referenced_name(connection, &mut key.referenced_table)?;
            Ok(key)
        })
        .collect()
}

fn declared_primary_key(connection: &Connection, table: &str) -> DbResult<Vec<String>> {
    let mut statement = connection
        .prepare("SELECT name FROM pragma_table_info(?1, 'main') WHERE pk > 0 ORDER BY pk")
        .map_err(map_error)?;

    statement
        .query_map(params![table], |row| row.get::<_, String>(0))
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)
}

/// Foreign keys may name the parent with different letter case; use the
/// schema's spelling so navigation finds the object.
fn canonicalize_referenced_name(connection: &Connection, object: &mut ObjectRef) -> DbResult<()> {
    let found = connection
        .query_row(
            "SELECT name, type FROM main.sqlite_schema \
             WHERE type IN ('table', 'view') AND name = ?1 COLLATE NOCASE",
            params![object.name],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(map_error)?;

    if let Some((name, kind)) = found {
        object.name = name;
        object.kind = object_kind(&kind);
    }

    Ok(())
}
