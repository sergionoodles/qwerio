//! Creating databases, tables, and columns: which changes a location allows,
//! and applying one on a short-lived writable session.

use std::sync::Arc;

use qwerio_core::capabilities::DatabaseCapabilities;
use qwerio_core::ddl::SchemaChange;
use qwerio_core::driver::{DatabaseSession, QueryRequest};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::{CatalogPath, ObjectKind, ObjectRef};
use qwerio_core::result::RequestEventKind;
use qwerio_core::session::AccessMode;
use qwerio_workspace::explorer::Location;

use crate::services::ResolvedConnection;
use crate::sessions::SessionRegistry;

/// Where each kind of schema change would apply from a location. `None`
/// means the change is unavailable there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaTargets {
    /// The connection a new database belongs to.
    pub database: Option<ConnectionId>,
    /// The connection and container a new table is created in.
    pub table: Option<(ConnectionId, CatalogPath)>,
    /// The table a new column is added to.
    pub column: Option<ObjectRef>,
}

/// The connection a location belongs to, if any.
pub fn location_connection(location: &Location) -> Option<ConnectionId> {
    match location {
        Location::Container { connection, .. } => Some(*connection),
        Location::Object(object) => Some(object.connection),
        _ => None,
    }
}

/// The changes available at `location`. `capabilities` belong to the
/// location's connection; `None` when that connection cannot be used here.
pub fn schema_targets(
    location: &Location,
    capabilities: Option<&DatabaseCapabilities>,
) -> SchemaTargets {
    let (Some(connection), Some(capabilities)) = (location_connection(location), capabilities)
    else {
        return SchemaTargets::default();
    };

    let container = match location {
        Location::Container { path, .. } => Some(path.clone()),
        Location::Object(object) => Some(object.container()),
        _ => None,
    };
    let table = container
        .filter(|path| holds_tables(path, capabilities))
        .map(|path| (connection, path));
    let column = match location {
        Location::Object(object) if object.kind == ObjectKind::Table => Some(object.clone()),
        _ => None,
    };

    SchemaTargets {
        database: Some(connection),
        table,
        column,
    }
}

/// Tables live at the deepest catalog level the engine has.
fn holds_tables(path: &CatalogPath, capabilities: &DatabaseCapabilities) -> bool {
    let has_database = path.database.is_some() || !capabilities.has_databases;
    let has_namespace = path.namespace.is_some() || !capabilities.has_namespaces;
    has_database && has_namespace
}

/// The database a change's session connects to.
fn session_database(change: &SchemaChange) -> Option<String> {
    match change {
        SchemaChange::CreateDatabase { .. } => None,
        SchemaChange::CreateTable { container, .. } => container.database.clone(),
        SchemaChange::AddColumn { table, .. } => table.database.clone(),
    }
}

/// Renders the change and runs it in autocommit on its own writable
/// session, which is closed afterwards.
pub async fn apply_schema_change(
    sessions: &Arc<SessionRegistry>,
    connection: ResolvedConnection,
    change: &SchemaChange,
) -> DbResult<()> {
    let sql = change.render_for_engine(connection.descriptor.engine())?;
    let lease = sessions
        .open_console(
            connection.descriptor,
            connection.credentials,
            session_database(change),
            AccessMode::ReadWrite,
        )
        .await?;

    let outcome = run_to_completion(lease.session().as_ref(), sql).await;
    let closed = lease.session().close().await;
    outcome.and(closed)
}

async fn run_to_completion(session: &dyn DatabaseSession, sql: String) -> DbResult<()> {
    let mut stream = session.execute(QueryRequest::new(sql))?;
    let mut failure = None;

    while let Some(event) = stream.events.recv().await {
        match event.kind {
            RequestEventKind::Failed(error) => failure = Some(error),
            RequestEventKind::Cancelled => failure = Some(DatabaseError::cancelled()),
            RequestEventKind::Finished { .. } => return failure.map_or(Ok(()), Err),
            _ => {}
        }
    }

    Err(failure.unwrap_or_else(|| {
        DatabaseError::new(ErrorKind::Internal, "The statement ended without a result")
    }))
}

#[cfg(test)]
mod tests {
    use qwerio_core::capabilities::CancellationSupport;
    use qwerio_core::session::ReadOnlyEnforcement;
    use qwerio_workspace::manifest::FolderId;

    use super::*;

    fn capabilities(has_databases: bool, has_namespaces: bool) -> DatabaseCapabilities {
        DatabaseCapabilities {
            has_databases,
            has_namespaces,
            access_modes: vec![AccessMode::ReadOnly, AccessMode::ReadWrite],
            read_only_enforcement: ReadOnlyEnforcement::NotEnforced,
            transactional_ddl: true,
            cancellation: CancellationSupport::Interrupt,
            filter_operators: Vec::new(),
            unicode_case_folding: false,
            errors_abort_transactions: false,
            column_types: vec!["text".to_owned()],
            key_column_type: "integer".to_owned(),
        }
    }

    fn container(connection: ConnectionId, path: CatalogPath) -> Location {
        Location::Container { connection, path }
    }

    fn object(connection: ConnectionId, kind: ObjectKind) -> ObjectRef {
        ObjectRef {
            connection,
            database: Some("app".to_owned()),
            namespace: Some("public".to_owned()),
            kind,
            name: "customers".to_owned(),
        }
    }

    #[test]
    fn tables_go_in_schemas_when_the_engine_has_them() {
        let connection = ConnectionId::new();
        let server = capabilities(true, true);
        let schema = CatalogPath::namespace(Some("app".to_owned()), "public");

        let at_root = schema_targets(&Location::connection_root(connection), Some(&server));
        let at_database = schema_targets(
            &container(connection, CatalogPath::database("app")),
            Some(&server),
        );
        let at_schema = schema_targets(&container(connection, schema.clone()), Some(&server));

        assert_eq!(
            at_root,
            SchemaTargets {
                database: Some(connection),
                table: None,
                column: None,
            }
        );
        assert_eq!(at_database.table, None);
        assert_eq!(at_schema.table, Some((connection, schema)));
        assert_eq!(at_schema.column, None);
    }

    #[test]
    fn tables_go_in_the_root_of_a_single_file_database() {
        let connection = ConnectionId::new();
        let targets = schema_targets(
            &Location::connection_root(connection),
            Some(&capabilities(false, false)),
        );

        assert_eq!(targets.table, Some((connection, CatalogPath::root())));
    }

    #[test]
    fn a_table_offers_a_column_and_a_sibling_table() {
        let connection = ConnectionId::new();
        let table = object(connection, ObjectKind::Table);
        let view = object(connection, ObjectKind::View);
        let server = capabilities(true, true);

        let at_table = schema_targets(&Location::Object(table.clone()), Some(&server));
        let at_view = schema_targets(&Location::Object(view), Some(&server));

        assert_eq!(at_table.column, Some(table.clone()));
        assert_eq!(at_table.table, Some((connection, table.container())));
        assert_eq!(at_view.column, None);
    }

    #[test]
    fn nothing_applies_outside_a_usable_connection() {
        let connection = ConnectionId::new();

        assert_eq!(
            schema_targets(
                &Location::Folder(FolderId::new()),
                Some(&capabilities(false, false))
            ),
            SchemaTargets::default()
        );
        assert_eq!(
            schema_targets(&Location::connection_root(connection), None),
            SchemaTargets::default()
        );
    }
}
