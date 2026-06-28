use pgsafe::{lint_sql, LintOptions};

fn fires(sql: &str) -> bool {
    lint_sql(sql, &LintOptions::default())
        .unwrap()
        .iter()
        .any(|f| f.rule_id == "require-timeout")
}

fn fires_assumed(sql: &str) -> bool {
    let mut opts = LintOptions::default();
    opts.assume_in_transaction = true;
    lint_sql(sql, &opts)
        .unwrap()
        .iter()
        .any(|f| f.rule_id == "require-timeout")
}

#[test]
fn unguarded_alter_table_fires() {
    assert!(fires("ALTER TABLE t ADD COLUMN c int;"));
    assert!(fires("DROP TABLE t;"));
    assert!(fires("DROP MATERIALIZED VIEW mv;"));
    assert!(fires("TRUNCATE t;"));
    assert!(fires("VACUUM FULL t;"));
    assert!(fires("CREATE INDEX i ON t (x);"));
}

#[test]
fn a_preceding_set_timeout_silences_it() {
    assert!(!fires(
        "SET lock_timeout = '5s'; ALTER TABLE t ADD COLUMN c int;"
    ));
    assert!(!fires("SET statement_timeout = '5s'; DROP TABLE t;"));
}

#[test]
fn concurrent_forms_and_plain_vacuum_are_silent() {
    assert!(!fires("CREATE INDEX CONCURRENTLY i ON t (x);"));
    assert!(!fires("VACUUM t;"));
}

#[test]
fn require_timeout_is_suppressible() {
    let fs = lint_sql(
        "-- pgsafe:ignore require-timeout  runs in a maintenance window\nALTER TABLE t ADD COLUMN c int;",
        &LintOptions::default(),
    )
    .unwrap();
    let f = fs.iter().find(|f| f.rule_id == "require-timeout").unwrap();
    assert!(f.is_suppressed());
    // a valid directive must not itself raise a suppression-* diagnostic
    assert!(fs.iter().all(|f| !f.rule_id.starts_with("suppression-")));
}

#[test]
fn set_local_at_top_of_file_is_scoped_by_in_transaction() {
    // Without the flag, a top-of-file SET LOCAL is a no-op → still fires.
    assert!(fires(
        "SET LOCAL lock_timeout = '5s'; ALTER TABLE t ADD COLUMN c int;"
    ));
    // With --in-transaction (implicit wrap), the SET LOCAL is in scope → silent.
    assert!(!fires_assumed(
        "SET LOCAL lock_timeout = '5s'; ALTER TABLE t ADD COLUMN c int;"
    ));
}
