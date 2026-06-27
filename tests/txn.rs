use pgsafe::{lint_sql, LintOptions};

fn fires(sql: &str, rule_id: &str) -> bool {
    lint_sql(sql, &LintOptions::default())
        .unwrap()
        .iter()
        .any(|f| f.rule_id == rule_id)
}

#[test]
fn concurrently_in_explicit_transaction_fires() {
    assert!(fires(
        "BEGIN; CREATE INDEX CONCURRENTLY i ON t (x); COMMIT;",
        "concurrently-in-transaction"
    ));
    assert!(fires(
        "BEGIN; DROP INDEX CONCURRENTLY i; COMMIT;",
        "concurrently-in-transaction"
    ));
    assert!(fires(
        "START TRANSACTION; REINDEX INDEX CONCURRENTLY i; COMMIT;",
        "concurrently-in-transaction"
    ));
}

#[test]
fn concurrently_outside_transaction_is_silent() {
    // no surrounding BEGIN → not in a transaction
    assert!(!fires(
        "CREATE INDEX CONCURRENTLY i ON t (x);",
        "concurrently-in-transaction"
    ));
    // after COMMIT → not in a transaction
    assert!(!fires(
        "BEGIN; COMMIT; CREATE INDEX CONCURRENTLY i ON t (x);",
        "concurrently-in-transaction"
    ));
    // non-concurrent index inside a txn is fine for THIS rule
    assert!(!fires(
        "BEGIN; CREATE INDEX i ON t (x); COMMIT;",
        "concurrently-in-transaction"
    ));
}

#[test]
fn concurrently_in_txn_is_exempt_from_new_table_dropping() {
    // foo is new+empty, but a CONCURRENTLY op in a txn fails regardless → still fires.
    let fs = lint_sql(
        "BEGIN; CREATE TABLE foo (id int); CREATE INDEX CONCURRENTLY i ON foo (id); COMMIT;",
        &LintOptions::default(),
    )
    .unwrap();
    assert!(fs
        .iter()
        .any(|f| f.rule_id == "concurrently-in-transaction"));
    // add-index-non-concurrent must NOT fire (it's CONCURRENTLY)
    assert!(!fs.iter().any(|f| f.rule_id == "add-index-non-concurrent"));
}

#[test]
fn concurrently_in_transaction_is_suppressible() {
    let fs = lint_sql(
        "BEGIN;\n-- pgsafe:ignore concurrently-in-transaction  tool runs outside a txn\nCREATE INDEX CONCURRENTLY i ON t (x);\nCOMMIT;",
        &LintOptions::default(),
    )
    .unwrap();
    let f = fs
        .iter()
        .find(|f| f.rule_id == "concurrently-in-transaction")
        .unwrap();
    assert!(f.is_suppressed());
    assert!(fs.iter().all(|f| !f.rule_id.starts_with("suppression-")));
}
