mod common;
qwerio_core::contract_tests!(common::MysqlFixture);

#[tokio::test(flavor = "multi_thread")]
async fn reports_session_backslash_mode_before_finished() {
    use qwerio_core::contract::{run, with_database};
    use qwerio_core::result::RequestEventKind;

    with_database(&common::MysqlFixture, |context| async move {
        let session = context.console().await;
        for (sql, expected, has_error) in [
            ("SET SESSION sql_mode = ''", false, false),
            ("SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'", true, false),
            ("SET SESSION sql_mode = 'not_a_real_mode'", true, true),
            ("SET SESSION sql_mode = ''", false, false),
        ] {
            let events = run(session.as_ref(), sql).await;
            assert_eq!(
                events
                    .iter()
                    .any(|event| matches!(event.kind, RequestEventKind::Failed(_))),
                has_error
            );
            assert_eq!(session.has_no_backslash_escapes(), expected);
        }
        session.close().await.expect("close session");
    })
    .await;
}

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
