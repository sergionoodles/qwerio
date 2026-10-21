//! Read back edits under row locks, since MySQL has no DML RETURNING.

use crate::dialect::MysqlDialect;
use crate::error::map_error;
use crate::{catalog, decode};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Row as MysqlRow};
use qwerio_core::catalog::{ColumnInfo, ObjectDetails};
use qwerio_core::edit::{
    RowChange, RowEditRequest, RowIdentity, SavedRow, check_editable, saved_row,
};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::sql::{
    ColumnRef, CompareOp, OBJECT_ALIAS, Predicate, SqlDialect, SqlRenderer, row_identity,
};
use qwerio_core::value::Value;

pub(crate) async fn apply(client: &mut Conn, request: &RowEditRequest) -> DbResult<Vec<SavedRow>> {
    check_editable(&request.details)
        .map_err(|reason| DatabaseError::new(ErrorKind::Unsupported, reason.message()))?;
    catalog::ensure_transactional(client, &request.details.object).await?;
    client
        .query_drop("START TRANSACTION")
        .await
        .map_err(map_error)?;
    let outcome = apply_changes(client, request).await;
    match outcome {
        Ok(rows) => {
            client.query_drop("COMMIT").await.map_err(map_error)?;
            Ok(rows)
        }
        Err(error) => {
            client.query_drop("ROLLBACK").await.map_err(map_error)?;
            Err(error)
        }
    }
}

async fn apply_changes(client: &mut Conn, request: &RowEditRequest) -> DbResult<Vec<SavedRow>> {
    let mut rows = Vec::with_capacity(request.changes.len());
    for change in &request.changes {
        rows.push(apply_change(client, &request.details, change).await?);
    }
    Ok(rows)
}

async fn apply_change(
    client: &mut Conn,
    details: &ObjectDetails,
    change: &RowChange,
) -> DbResult<SavedRow> {
    let table = MysqlDialect.qualified_name(&details.object);
    let alias = MysqlDialect.quote_identifier(OBJECT_ALIAS);
    match change {
        RowChange::Insert { values } => {
            let values = resolve_key_defaults(client, details, values).await?;
            let (names, placeholders, params) = assignments(details, &values)?;
            let sql = format!(
                "INSERT INTO {table} ({}) VALUES ({})",
                names.join(", "),
                placeholders.join(", ")
            );
            client.exec_drop(sql, params).await.map_err(map_error)?;
            let inserted_id = client.last_insert_id();
            let identity = inserted_identity(client, details, &values, inserted_id).await?;
            read_row(client, details, &identity).await
        }
        RowChange::Update(update) => {
            // Select under a lock first. This also distinguishes an unchanged
            // row from a missing row independently of affected-row settings.
            read_row(client, details, &update.identity).await?;
            if update.changes.is_empty() {
                return Err(DatabaseError::invalid_request(
                    "The update changes no column",
                ));
            }
            let (names, placeholders, _) = assignments(details, &update.changes)?;
            let mut renderer = SqlRenderer::new(&MysqlDialect, details);
            let assignments = names
                .iter()
                .zip(&placeholders)
                .zip(&update.changes)
                .map(|((name, _), (column, value))| {
                    let placeholder = renderer.bind(value.clone(), details.column(column));
                    format!("{name} = {placeholder}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            let predicate = identity_predicate(details, &update.identity)?;
            let where_clause = renderer.render_predicate(&predicate)?;
            client
                .exec_drop(
                    format!("UPDATE {table} AS {alias} SET {assignments} WHERE {where_clause}"),
                    decode::parameters(&renderer.into_params())?,
                )
                .await
                .map_err(map_error)?;
            let identity = update
                .identity
                .iter()
                .map(|(column, old)| {
                    let new = match column {
                        ColumnRef::Column(name) => update
                            .changes
                            .iter()
                            .find(|(changed, _)| changed == name)
                            .map(|(_, value)| value.clone()),
                        ColumnRef::Locator => None,
                    };
                    (column.clone(), new.unwrap_or_else(|| old.clone()))
                })
                .collect();
            read_row(client, details, &identity).await
        }
        RowChange::Delete { identity } => {
            let row = read_row(client, details, identity).await?;
            let mut renderer = SqlRenderer::new(&MysqlDialect, details);
            let where_clause =
                renderer.render_predicate(&identity_predicate(details, identity)?)?;
            // Multi-table alias syntax works on both MySQL and older MariaDB.
            client
                .exec_drop(
                    format!("DELETE {alias} FROM {table} AS {alias} WHERE {where_clause}"),
                    decode::parameters(&renderer.into_params())?,
                )
                .await
                .map_err(map_error)?;
            if client.affected_rows() != 1 {
                return Err(DatabaseError::internal(
                    "The delete did not affect exactly one row",
                ));
            }
            Ok(row)
        }
    }
}

fn assignments(
    details: &ObjectDetails,
    values: &[(String, Value)],
) -> DbResult<(Vec<String>, Vec<String>, Vec<mysql_async::Value>)> {
    let mut renderer = SqlRenderer::new(&MysqlDialect, details);
    let mut names = Vec::with_capacity(values.len());
    let mut placeholders = Vec::with_capacity(values.len());
    for (index, (name, value)) in values.iter().enumerate() {
        let column = details
            .column(name)
            .ok_or_else(|| DatabaseError::invalid_request(format!("Unknown column \"{name}\"")))?;
        if column.generated {
            return Err(DatabaseError::invalid_request(format!(
                "Column \"{name}\" is generated by the database"
            )));
        }
        if values[..index].iter().any(|(earlier, _)| earlier == name) {
            return Err(DatabaseError::invalid_request(format!(
                "Column \"{name}\" is set twice"
            )));
        }
        names.push(MysqlDialect.quote_identifier(name));
        placeholders.push(renderer.bind(value.clone(), Some(column)));
    }
    Ok((
        names,
        placeholders,
        decode::parameters(&renderer.into_params())?,
    ))
}

fn identity_predicate(details: &ObjectDetails, identity: &RowIdentity) -> DbResult<Predicate> {
    let expected = row_identity(details)
        .ok_or_else(|| DatabaseError::invalid_request("This table has no unique row key"))?;
    if identity.len() != expected.len()
        || expected.iter().any(|column| {
            identity
                .iter()
                .filter(|(candidate, value)| candidate == column && !value.is_null())
                .count()
                != 1
        })
    {
        return Err(DatabaseError::invalid_request(
            "The row must be identified by its complete NULL-free unique key",
        ));
    }
    Ok(Predicate::and(
        identity
            .iter()
            .map(|(column, value)| Predicate::Compare {
                column: column.clone(),
                op: CompareOp::Equal,
                value: value.clone(),
            })
            .collect(),
    ))
}

async fn read_row(
    client: &mut Conn,
    details: &ObjectDetails,
    identity: &RowIdentity,
) -> DbResult<SavedRow> {
    let mut renderer = SqlRenderer::new(&MysqlDialect, details);
    let where_clause = renderer.render_predicate(&identity_predicate(details, identity)?)?;
    let columns = details
        .columns
        .iter()
        .map(|column| MysqlDialect.quote_identifier(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {columns} FROM {} AS {} WHERE {where_clause} LIMIT 2 FOR UPDATE",
        MysqlDialect.qualified_name(&details.object),
        MysqlDialect.quote_identifier(OBJECT_ALIAS)
    );
    let rows: Vec<MysqlRow> = client
        .exec(sql, decode::parameters(&renderer.into_params())?)
        .await
        .map_err(map_error)?;
    saved_row(details, rows.into_iter().map(decode::decode_row).collect())
}

async fn inserted_identity(
    client: &mut Conn,
    details: &ObjectDetails,
    values: &[(String, Value)],
    inserted_id: Option<u64>,
) -> DbResult<RowIdentity> {
    let key = row_identity(details)
        .ok_or_else(|| DatabaseError::invalid_request("This table has no unique row key"))?;
    let generated_column: Option<String> = if inserted_id.is_some() {
        client.exec_first("SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND EXTRA LIKE '%auto_increment%'",
            (catalog::database(&details.object)?, &details.object.name)).await.map_err(map_error)?
    } else {
        None
    };
    key.into_iter()
        .map(|column| {
            let ColumnRef::Column(name) = &column else {
                return Err(DatabaseError::invalid_request(
                    "MySQL has no hidden row locator",
                ));
            };
            // A generated ID wins when NULL/zero requested auto-increment.
            let value = if generated_column.as_ref() == Some(name) {
                inserted_id.map(|value| {
                    i64::try_from(value)
                        .map(Value::Integer)
                        .unwrap_or_else(|_| Value::Decimal(value.to_string()))
                })
            } else {
                values
                    .iter()
                    .find(|(candidate, _)| candidate == name)
                    .map(|(_, value)| value.clone())
            };
            value.map(|value| (column, value)).ok_or_else(|| {
                DatabaseError::new(
                    ErrorKind::Unsupported,
                    "The generated row key cannot be read back safely; supply its value explicitly",
                )
            })
        })
        .collect()
}

/// Evaluate non-integer key defaults once, then include that value in INSERT.
/// This gives UUID()/other expression defaults a stable identity without
/// session variables or a second evaluation of a volatile default.
async fn resolve_key_defaults(
    client: &mut Conn,
    details: &ObjectDetails,
    values: &[(String, Value)],
) -> DbResult<Vec<(String, Value)>> {
    let mut resolved = values.to_vec();
    for column in &details
        .unique_row_key()
        .ok_or_else(|| DatabaseError::invalid_request("This table has no unique key"))?
        .columns
    {
        if resolved.iter().any(|(name, _)| name == column) {
            continue;
        }
        let info = details
            .column(column)
            .ok_or_else(|| DatabaseError::invalid_request("The row key column no longer exists"))?;
        let Some(default) = &info.default else {
            continue;
        };
        let extra: Option<String> = client.exec_first("SELECT EXTRA FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND COLUMN_NAME = ?",
            (catalog::database(&details.object)?, &details.object.name, column)).await.map_err(map_error)?;
        let version: Option<String> = client
            .query_first("SELECT VERSION()")
            .await
            .map_err(map_error)?;
        let is_expression = extra
            .as_deref()
            .is_some_and(|extra| extra.contains("DEFAULT_GENERATED"))
            || version
                .as_deref()
                .is_some_and(|version| version.contains("MariaDB"));
        let value = if is_expression {
            evaluate_default(client, details, default, &resolved).await?
        } else {
            Value::Text(default.clone())
        };
        resolved.push((column.clone(), value));
    }
    Ok(resolved)
}

async fn evaluate_default(
    client: &mut Conn,
    details: &ObjectDetails,
    expression: &str,
    values: &[(String, Value)],
) -> DbResult<Value> {
    let inputs = values
        .iter()
        .map(|(name, _)| {
            let column = details.column(name).ok_or_else(|| {
                DatabaseError::invalid_request(format!("Unknown column \"{name}\""))
            })?;
            Ok(format!(
                "{} AS {}",
                default_input(column),
                MysqlDialect.quote_identifier(name)
            ))
        })
        .collect::<DbResult<Vec<_>>>()?
        .join(", ");
    let source = if inputs.is_empty() {
        String::new()
    } else {
        format!(" FROM (SELECT {inputs}) AS qwerio_defaults")
    };
    let sql = format!("SELECT {expression}{source}");
    let params = decode::parameters(
        &values
            .iter()
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>(),
    )?;
    // Binary protocol also ensures metadata-derived expressions contain no
    // additional statements, even if a default contains quoted semicolons.
    let row: Option<MysqlRow> = client.exec_first(sql, params).await.map_err(map_error)?;
    row.and_then(|row| decode::decode_row(row).into_iter().next())
        .ok_or_else(|| DatabaseError::internal("The key default returned no value"))
}

fn default_input(column: &ColumnInfo) -> String {
    let type_name = column.type_name.to_ascii_lowercase();
    let base = type_name.split(['(', ' ']).next().unwrap_or_default();
    let cast = match base {
        // MariaDB otherwise coerces a text-bound decimal to DOUBLE when an
        // expression uses it, losing integer precision above 2^53.
        "decimal" | "numeric" => type_name.split_whitespace().next(),
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "year" => {
            Some(if type_name.contains("unsigned") {
                "UNSIGNED"
            } else {
                "SIGNED"
            })
        }
        _ => None,
    };
    match cast {
        Some(cast) => format!("CAST(? AS {cast})"),
        None => "?".to_owned(),
    }
}
