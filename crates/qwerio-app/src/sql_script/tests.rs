use qwerio_core::connection::EngineKind;
use qwerio_core::error::{DatabaseError, ErrorKind};

use super::*;

fn split(sql: &str, engine: EngineKind) -> Vec<ScriptStatement> {
    split_with_mode(sql, engine, MysqlSqlMode::default())
}

fn split_with_mode(sql: &str, engine: EngineKind, mode: MysqlSqlMode) -> Vec<ScriptStatement> {
    let mut run = SqlRun::script(sql, engine).expect("plan script");
    let mut statements = Vec::new();
    while let Some(statement) = run.next_statement(mode).expect("split script") {
        statements.push(statement);
        run.complete_statement(&RunOutcome::Succeeded);
    }
    assert_eq!(run.total(), Some(statements.len()));
    statements
}

#[test]
fn splits_statements_and_preserves_document_byte_offsets() {
    let sql = "-- é;\nSELECT 'a;''b';\n/* ; */ SELECT 2; ; -- done";
    let statements = split(sql, EngineKind::Postgres);
    assert_eq!(statements.len(), 2);
    assert_eq!(statements[0].sql, "-- é;\nSELECT 'a;''b';");
    assert_eq!(statements[1].sql, "\n/* ; */ SELECT 2;");
    for statement in statements {
        assert_eq!(
            &sql[statement.start..statement.start + statement.sql.len()],
            statement.sql
        );
    }
}

#[test]
fn postgres_bodies_nested_comments_and_escape_strings_stay_intact() {
    let sql = r#"DO $body$ BEGIN RAISE NOTICE ';'; END $body$;
        /* outer /* ; */ inner */ SELECT E'escaped\';semi', "a;b";
        SELECT $$x;y$$; SELECT name$part FROM example;"#;
    let statements = split(sql, EngineKind::Postgres);
    assert_eq!(statements.len(), 4);
    assert!(statements[0].sql.contains("END $body$"));
    assert!(statements[1].sql.contains(r"E'escaped\';semi'"));
    assert!(statements[2].sql.contains("$$x;y$$"));
    assert!(statements[3].sql.contains("name$part"));
}

#[test]
fn sqlite_triggers_case_expressions_and_quoted_identifiers_stay_intact() {
    let sql = "CREATE TEMP TRIGGER audit AFTER INSERT ON customers BEGIN
        INSERT INTO log VALUES (CASE WHEN NEW.id > 0 THEN 'a;b' ELSE 'c' END);
        UPDATE log SET [a;b] = 1;
        END; SELECT `a;b` FROM log;";
    let statements = split(sql, EngineKind::Sqlite);
    assert_eq!(statements.len(), 2);
    assert!(statements[0].sql.ends_with("END;"));
    assert!(statements[1].sql.contains("SELECT `a;b`"));
}

#[test]
fn mysql_delimiters_quotes_and_comment_rules_are_respected() {
    for engine in [EngineKind::Mysql, EngineKind::Mariadb] {
        let sql = "# ;\nDELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 'a\\\';b'; SELECT 2; END$$\nDELIMITER ;\nSELECT 5--2; /*! SET @x = 1 */; # done";
        let statements = split(sql, engine);
        assert_eq!(statements.len(), 3);
        assert!(statements[0].sql.starts_with("CREATE PROCEDURE"));
        assert!(statements[0].sql.ends_with("END"));
        assert!(!statements[0].sql.contains("DELIMITER"));
        assert!(statements[1].sql.contains("SELECT 5--2"));
        assert!(statements[2].sql.contains("/*! SET"));
    }
}

#[test]
fn sqlite_backslashes_do_not_escape_quotes_and_postgres_hashes_are_operators() {
    assert_eq!(split(r"SELECT '\'; SELECT 2;", EngineKind::Sqlite).len(), 2);
    assert_eq!(
        split("SELECT 4 # 2; SELECT 2", EngineKind::Postgres).len(),
        2
    );
}

#[test]
fn rejects_empty_or_unterminated_scripts_before_execution() {
    for (sql, engine, kind) in [
        (
            " ; -- note\n/* comment */",
            EngineKind::Sqlite,
            ErrorKind::EmptyStatement,
        ),
        ("# note\n;", EngineKind::Mysql, ErrorKind::EmptyStatement),
        (
            "SELECT 1; SELECT 'unclosed",
            EngineKind::Sqlite,
            ErrorKind::Syntax,
        ),
        (
            "SELECT 1; /* unclosed",
            EngineKind::Postgres,
            ErrorKind::Syntax,
        ),
        (
            "SELECT $body$unclosed",
            EngineKind::Postgres,
            ErrorKind::Syntax,
        ),
        ("DELIMITER \nSELECT 1", EngineKind::Mysql, ErrorKind::Syntax),
    ] {
        assert_eq!(
            SqlRun::script(sql, engine)
                .and_then(|mut run| run.next_statement(MysqlSqlMode::default()))
                .expect_err("invalid script")
                .kind,
            kind,
            "{sql}"
        );
    }
}

#[test]
fn advances_only_after_successful_completion() {
    let mut run = SqlRun::script("SELECT 1; SELECT 2", EngineKind::Sqlite).expect("script");
    assert_eq!(run.total(), Some(2));
    assert!(
        run.next_statement(MysqlSqlMode::default())
            .expect("first")
            .is_some()
    );
    assert!(
        run.next_statement(MysqlSqlMode::default())
            .expect("active")
            .is_none()
    );
    run.complete_statement(&RunOutcome::Succeeded);
    assert_eq!(run.completed(), 1);
    assert!(
        run.next_statement(MysqlSqlMode::default())
            .expect("second")
            .is_some()
    );
    run.complete_statement(&RunOutcome::Succeeded);
    assert_eq!(run.completed(), 2);
    assert_eq!(run.started(), 2);
    assert!(
        run.next_statement(MysqlSqlMode::default())
            .expect("end")
            .is_none()
    );
}

#[test]
fn failure_cancellation_and_late_cancel_requests_stop_the_script() {
    for outcome in [
        RunOutcome::Failed(DatabaseError::new(ErrorKind::Syntax, "bad statement")),
        RunOutcome::Cancelled,
    ] {
        let mut run = SqlRun::script("SELECT 1; SELECT 2", EngineKind::Sqlite).expect("script");
        assert!(
            run.next_statement(MysqlSqlMode::default())
                .expect("first")
                .is_some()
        );
        run.complete_statement(&outcome);
        assert!(
            run.next_statement(MysqlSqlMode::default())
                .expect("stopped")
                .is_none()
        );
        assert_eq!(run.completed(), 0);
    }

    let mut run = SqlRun::script("SELECT 1; SELECT 2", EngineKind::Sqlite).expect("script");
    assert!(
        run.next_statement(MysqlSqlMode::default())
            .expect("first")
            .is_some()
    );
    run.stop();
    run.complete_statement(&RunOutcome::Succeeded);
    assert!(
        run.next_statement(MysqlSqlMode::default())
            .expect("stopped")
            .is_none()
    );
}

#[test]
fn sqlite_trigger_identifiers_do_not_change_the_body_boundaries() {
    for trigger_name in ["audit", "begin", "end"] {
        let sql = format!(
            "CREATE TABLE items (end INTEGER); CREATE TABLE log (value INTEGER);
             CREATE TRIGGER {trigger_name} AFTER INSERT ON items BEGIN
                 INSERT INTO log VALUES (NEW.end);
                 INSERT INTO log VALUES (CASE WHEN NEW.end > 0 THEN NEW.end ELSE 0 END);
             END;
             INSERT INTO items VALUES (9); SELECT value FROM log;"
        );
        let statements = split(&sql, EngineKind::Sqlite);
        assert_eq!(statements.len(), 5, "{trigger_name}");

        let connection = rusqlite::Connection::open_in_memory().expect("database");
        for statement in &statements[..4] {
            connection.execute_batch(&statement.sql).expect("statement");
        }
        let rows = connection
            .prepare(&statements[4].sql)
            .expect("select log")
            .query_map([], |row| row.get::<_, i64>(0))
            .expect("query log")
            .collect::<Result<Vec<_>, _>>()
            .expect("log values");
        assert_eq!(rows, vec![9, 9]);
    }
}

#[test]
fn postgres_atomic_function_and_procedure_bodies_stay_intact() {
    for kind in ["FUNCTION answer() RETURNS integer", "PROCEDURE answer()"] {
        let definition = format!(
            "CREATE {kind} LANGUAGE SQL BEGIN /* body */ ATOMIC
                 SELECT CASE WHEN 1 > 0 THEN CASE WHEN true THEN 42 ELSE 0 END ELSE 1 END;
                 SELECT 42;
             END;"
        );
        let statements = split(&format!("{definition} SELECT 2;"), EngineKind::Postgres);
        assert_eq!(statements.len(), 2);
        assert_eq!(statements[0].sql, definition);
        assert_eq!(statements[1].sql.trim(), "SELECT 2;");
    }
}

#[test]
fn postgres_backticks_are_operator_characters() {
    let statements = split(
        "CREATE OPERATOR ` (FUNCTION = int4pl, LEFTARG = integer, RIGHTARG = integer);
         SELECT 1 ` 2; SELECT 3 ` 4;",
        EngineKind::Postgres,
    );
    assert_eq!(statements.len(), 3);
    assert_eq!(statements[1].sql.trim(), "SELECT 1 ` 2;");
    assert_eq!(statements[2].sql.trim(), "SELECT 3 ` 4;");
}

#[test]
fn mysql_no_backslash_escapes_preserves_string_boundaries() {
    for engine in [EngineKind::Mysql, EngineKind::Mariadb] {
        let sql = r"SELECT '\'; SELECT 'a;''b'; SELECT 2;";
        let statements = split_with_mode(
            sql,
            engine,
            MysqlSqlMode {
                no_backslash_escapes: true,
            },
        );
        assert_eq!(statements.len(), 3);
        assert_eq!(statements[0].sql, r"SELECT '\';");
    }
}

#[test]
fn mysql_uses_new_quoting_rules_between_statements_and_preserves_offsets() {
    for engine in [EngineKind::Mysql, EngineKind::Mariadb] {
        let sql = r"-- é
            DELIMITER $$
            SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'$$
            SELECT '\'$$
            SET SESSION sql_mode = ''$$
            SELECT 'a\';b'$$
            DELIMITER ;
            SELECT 2;";
        let mut run = SqlRun::script(sql, engine).expect("script");
        assert_eq!(run.total(), None);
        let no_backslash_escapes = MysqlSqlMode {
            no_backslash_escapes: true,
        };
        for mode in [
            MysqlSqlMode::default(),
            no_backslash_escapes,
            no_backslash_escapes,
            MysqlSqlMode::default(),
            MysqlSqlMode::default(),
        ] {
            let statement = run
                .next_statement(mode)
                .expect("split next")
                .expect("statement");
            assert_eq!(
                &sql[statement.start..statement.start + statement.sql.len()],
                statement.sql
            );
            assert!(!statement.sql.contains("DELIMITER"));
            run.complete_statement(&RunOutcome::Succeeded);
        }
        assert!(
            run.next_statement(MysqlSqlMode::default())
                .expect("end")
                .is_none()
        );
        assert_eq!(run.total(), Some(5));
        assert_eq!(run.completed(), 5);
    }
}

#[test]
fn mysql_syntax_errors_keep_their_document_offsets() {
    let sql = "DELIMITER $$\nSELECT 1$$\nSELECT 'unclosed";
    let mut run = SqlRun::script(sql, EngineKind::Mysql).expect("script");
    run.next_statement(MysqlSqlMode::default()).expect("first");
    run.complete_statement(&RunOutcome::Succeeded);
    let error = run
        .next_statement(MysqlSqlMode::default())
        .expect_err("invalid quote");
    assert_eq!(error.kind, ErrorKind::Syntax);
    assert_eq!(
        error.position.expect("position").byte_offset,
        sql.find('\'').expect("quote")
    );
}

#[test]
fn mysql_stops_before_parsing_or_starting_later_statements() {
    for outcome in [RunOutcome::Cancelled, RunOutcome::Succeeded] {
        let mut run =
            SqlRun::script("SELECT 1; SELECT 'unclosed", EngineKind::Mysql).expect("script");
        run.next_statement(MysqlSqlMode::default()).expect("first");
        run.stop();
        run.complete_statement(&outcome);
        assert!(
            run.next_statement(MysqlSqlMode::default())
                .expect("stopped")
                .is_none()
        );
        assert_eq!(run.started(), 1);
    }
}
