use qwerio_core::connection::EngineKind;
use qwerio_core::error::{DatabaseError, ErrorKind};

use super::*;

fn split(sql: &str, engine: EngineKind) -> Vec<ScriptStatement> {
    split::statements(sql, engine).expect("split script")
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
                .err()
                .expect("invalid script")
                .kind,
            kind,
            "{sql}"
        );
    }
}

#[test]
fn advances_only_after_successful_completion() {
    let mut run = SqlRun::script("SELECT 1; SELECT 2", EngineKind::Sqlite).expect("script");
    assert_eq!(run.total(), 2);
    assert!(run.next_statement().is_some());
    assert!(run.next_statement().is_none());
    run.complete_statement(&RunOutcome::Succeeded);
    assert_eq!(run.completed(), 1);
    assert!(run.next_statement().is_some());
    run.complete_statement(&RunOutcome::Succeeded);
    assert_eq!(run.completed(), 2);
    assert_eq!(run.started(), 2);
    assert!(run.next_statement().is_none());
}

#[test]
fn failure_cancellation_and_late_cancel_requests_stop_the_script() {
    for outcome in [
        RunOutcome::Failed(DatabaseError::new(ErrorKind::Syntax, "bad statement")),
        RunOutcome::Cancelled,
    ] {
        let mut run = SqlRun::script("SELECT 1; SELECT 2", EngineKind::Sqlite).expect("script");
        assert!(run.next_statement().is_some());
        run.complete_statement(&outcome);
        assert!(run.next_statement().is_none());
        assert_eq!(run.completed(), 0);
    }

    let mut run = SqlRun::script("SELECT 1; SELECT 2", EngineKind::Sqlite).expect("script");
    assert!(run.next_statement().is_some());
    run.stop();
    run.complete_statement(&RunOutcome::Succeeded);
    assert!(run.next_statement().is_none());
}
