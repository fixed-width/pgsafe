use pgsafe::lint_sql;

fn ids(sql: &str) -> Vec<String> {
    lint_sql(sql)
        .unwrap()
        .into_iter()
        .map(|f| f.rule_id)
        .collect()
}
fn active_count(sql: &str) -> usize {
    lint_sql(sql)
        .unwrap()
        .iter()
        .filter(|f| !f.is_suppressed())
        .count()
}

#[test]
fn suppressed_finding_is_present_but_not_active() {
    let sql = "-- pgsafe:ignore drop-table  empty, confirmed off-peak\nDROP TABLE x;";
    assert!(lint_sql(sql)
        .unwrap()
        .iter()
        .find(|f| f.rule_id == "drop-table")
        .unwrap()
        .is_suppressed());
    assert_eq!(active_count(sql), 0);
}
#[test]
fn missing_reason_keeps_finding_active_and_adds_diagnostic() {
    let sql = "-- pgsafe:ignore drop-table\nDROP TABLE x;";
    assert!(ids(sql).contains(&"suppression-missing-reason".to_string()));
    assert!(active_count(sql) >= 1);
}
#[test]
fn unknown_rule_keeps_finding_active() {
    let sql = "-- pgsafe:ignore drop-tabel  typo\nDROP TABLE x;";
    assert!(ids(sql).contains(&"suppression-unknown-rule".to_string()));
    assert!(active_count(sql) >= 1);
}
#[test]
fn unused_directive_is_a_warning() {
    assert!(ids("-- pgsafe:ignore truncate  stale\nDELETE FROM x;")
        .contains(&"suppression-unused".to_string()));
}
#[test]
fn string_literal_lookalike_is_not_a_directive() {
    let sql = "SELECT '-- pgsafe:ignore drop-table x';\nDROP TABLE y;";
    let fs = lint_sql(sql).unwrap();
    assert!(fs
        .iter()
        .any(|f| f.rule_id == "drop-table" && !f.is_suppressed()));
    assert!(!fs.iter().any(|f| f.rule_id.starts_with("suppression-")));
}
#[test]
fn trailing_directive_suppresses() {
    assert_eq!(
        active_count("DROP TABLE x;  -- pgsafe:ignore drop-table  one-off cleanup"),
        0
    );
}
