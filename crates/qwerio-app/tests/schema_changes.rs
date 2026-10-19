//! Creating tables and columns through the real SQLite adapter.

use std::sync::Arc;

use qwerio_app::AppServices;
use qwerio_app::credentials::MemoryCredentialStore;
use qwerio_app::local_state::LocalState;
use qwerio_app::registry::DriverRegistry;
use qwerio_app::schema::apply_schema_change;
use qwerio_app::services::ResolvedConnection;
use qwerio_core::connection::{ConnectionOptions, EngineKind, SqliteOptions};
use qwerio_core::ddl::{ColumnDefinition, SchemaChange};
use qwerio_core::driver::DatabaseDriver;
use qwerio_core::resource::{CatalogEntryKind, CatalogPath, ObjectKind, ObjectRef};
use qwerio_sqlite::{SqliteDriver, create_database_file};
use qwerio_workspace::Workspace;
use tempfile::TempDir;

struct Environment {
    _directory: TempDir,
    services: AppServices,
    resolved: ResolvedConnection,
    key_column_type: String,
}

fn environment() -> Environment {
    let directory = TempDir::new().expect("temporary directory");
    let database = directory.path().join("shop.sqlite");
    create_database_file(&database).expect("create database");

    let driver = Arc::new(SqliteDriver::new());
    let key_column_type = driver.capabilities().key_column_type.clone();
    let mut drivers = DriverRegistry::new();
    drivers.register(driver);
    let services = AppServices::new(
        drivers,
        LocalState::in_memory().expect("local state"),
        Arc::new(MemoryCredentialStore::default()),
    );

    let workspace_root = directory.path().join("workspace");
    std::fs::create_dir(&workspace_root).expect("workspace folder");
    let mut workspace = Workspace::create(&workspace_root, "Shop").expect("workspace");
    let connection = workspace
        .add_connection("Shop data", EngineKind::Sqlite)
        .expect("alias");
    services
        .bind_connection(
            workspace.manifest(),
            connection,
            ConnectionOptions::Sqlite(SqliteOptions::new(&database)),
            None,
        )
        .expect("bind");
    let resolved = services
        .resolve_connection(workspace.manifest(), connection)
        .expect("resolve");

    Environment {
        _directory: directory,
        services,
        resolved,
        key_column_type,
    }
}

fn column(name: &str, data_type: &str) -> ColumnDefinition {
    ColumnDefinition {
        name: name.to_owned(),
        data_type: data_type.to_owned(),
        is_nullable: true,
        default: None,
        is_primary_key: false,
    }
}

fn customers(environment: &Environment) -> ObjectRef {
    ObjectRef {
        connection: environment.resolved.descriptor.id,
        database: None,
        namespace: None,
        kind: ObjectKind::Table,
        name: "customers".to_owned(),
    }
}

async fn apply(environment: &Environment, change: &SchemaChange) -> qwerio_core::DbResult<()> {
    apply_schema_change(
        &environment.services.sessions,
        environment.resolved.clone(),
        change,
    )
    .await
}

async fn column_names(environment: &Environment) -> Vec<String> {
    let session = environment
        .services
        .sessions
        .browse(
            &environment.resolved.descriptor,
            &environment.resolved.credentials,
            None,
        )
        .await
        .expect("browse session");
    let details = session
        .describe(&customers(environment))
        .await
        .expect("describe customers");

    details
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn creates_a_table_and_adds_a_column() {
    let environment = environment();
    let create = SchemaChange::CreateTable {
        container: CatalogPath::root(),
        name: "customers".to_owned(),
        columns: vec![
            ColumnDefinition {
                is_nullable: false,
                is_primary_key: true,
                ..column("id", &environment.key_column_type)
            },
            column("name", "TEXT"),
        ],
    };
    apply(&environment, &create).await.expect("create table");

    let session = environment
        .services
        .sessions
        .browse(
            &environment.resolved.descriptor,
            &environment.resolved.credentials,
            None,
        )
        .await
        .expect("browse session");
    let catalog = session.list(&CatalogPath::root()).await.expect("catalog");
    assert!(catalog.iter().any(|entry| entry.name == "customers"
        && entry.kind == CatalogEntryKind::Object(ObjectKind::Table)));

    let add = SchemaChange::AddColumn {
        table: customers(&environment),
        column: ColumnDefinition {
            is_nullable: false,
            default: Some("0".to_owned()),
            ..column("visits", "INTEGER")
        },
    };
    apply(&environment, &add).await.expect("add column");

    assert_eq!(column_names(&environment).await, ["id", "name", "visits"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_rejected_change_and_leaves_the_table_as_it_was() {
    let environment = environment();
    let create = SchemaChange::CreateTable {
        container: CatalogPath::root(),
        name: "customers".to_owned(),
        columns: vec![column("name", "TEXT")],
    };
    apply(&environment, &create).await.expect("create table");

    let duplicate = SchemaChange::AddColumn {
        table: customers(&environment),
        column: column("name", "TEXT"),
    };
    let error = apply(&environment, &duplicate)
        .await
        .expect_err("the column already exists");

    assert!(
        error.message.contains("duplicate column name"),
        "unexpected error: {}",
        error.message
    );
    assert_eq!(column_names(&environment).await, ["name"]);
}
