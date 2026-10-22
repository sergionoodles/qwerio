mod common;
qwerio_core::contract_tests!(common::MysqlFixture);

#[tokio::test(flavor = "multi_thread")]
async fn manages_users_roles_and_permissions() {
    use mysql_async::prelude::Queryable;
    qwerio_core::contract::with_database(
        &common::MysqlFixture,
        |context| async move {
            let database = common::MysqlFixture.engine().quote_identifier(context.database.as_deref().expect("database"));
            let mut admin = common::MysqlFixture.admin().await;
            admin.query_drop(format!("CREATE FUNCTION {database}.permission_function() RETURNS integer DETERMINISTIC RETURN 1")).await.expect("seed function through text protocol");
            admin.query_drop(format!("CREATE PROCEDURE {database}.permission_procedure() SELECT 1")).await.expect("seed procedure through text protocol");
            admin.disconnect().await.expect("close seed connection");
            qwerio_core::contract::manages_users_roles_and_permissions(context).await;
        },
    )
    .await;
}
