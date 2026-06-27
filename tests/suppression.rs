use pgsafe::{lint_sql, LintOptions};

fn ids(sql: &str) -> Vec<String> {
    lint_sql(sql, &LintOptions::default())
        .unwrap()
        .into_iter()
        .map(|f| f.rule_id)
        .collect()
}
fn active_count(sql: &str) -> usize {
    lint_sql(sql, &LintOptions::default())
        .unwrap()
        .iter()
        .filter(|f| !f.is_suppressed())
        .count()
}

#[test]
fn suppressed_finding_is_present_but_not_active() {
    // SET lock_timeout prevents require-timeout; only drop-table fires (suppressed).
    let sql =
        "SET lock_timeout = '5s';\n-- pgsafe:ignore drop-table  empty, confirmed off-peak\nDROP TABLE x;";
    assert!(lint_sql(sql, &LintOptions::default())
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
    assert!(active_count(sql) >= 2);
}
#[test]
fn unknown_rule_keeps_finding_active() {
    let sql = "-- pgsafe:ignore drop-tabel  typo\nDROP TABLE x;";
    assert!(ids(sql).contains(&"suppression-unknown-rule".to_string()));
    assert!(active_count(sql) >= 2);
}
#[test]
fn unused_directive_is_a_warning() {
    assert!(ids("-- pgsafe:ignore truncate  stale\nDELETE FROM x;")
        .contains(&"suppression-unused".to_string()));
}
#[test]
fn string_literal_lookalike_is_not_a_directive() {
    let sql = "SELECT '-- pgsafe:ignore drop-table x';\nDROP TABLE y;";
    let fs = lint_sql(sql, &LintOptions::default()).unwrap();
    assert!(fs
        .iter()
        .any(|f| f.rule_id == "drop-table" && !f.is_suppressed()));
    assert!(!fs.iter().any(|f| f.rule_id.starts_with("suppression-")));
}
#[test]
fn trailing_directive_suppresses() {
    // SET lock_timeout prevents require-timeout; only drop-table fires (suppressed).
    assert_eq!(
        active_count(
            "SET lock_timeout = '5s';\nDROP TABLE x;  -- pgsafe:ignore drop-table  one-off cleanup"
        ),
        0
    );
}
#[test]
fn multibyte_content_keeps_suppressed_finding_correct() {
    let sql = "SELECT 'café ☕';\n-- pgsafe:ignore drop-table  réson with ünïcode\nDROP TABLE x;";
    let fs = lint_sql(sql, &LintOptions::default()).unwrap();
    let dt = fs.iter().find(|f| f.rule_id == "drop-table").unwrap();
    assert!(dt.is_suppressed());
    assert_eq!(
        dt.suppression.as_ref().unwrap().reason,
        "réson with ünïcode"
    );
    assert_eq!(dt.snippet, "DROP TABLE x");
}
#[test]
fn trailing_directive_with_two_statements_on_a_line_attaches_to_the_rightmost() {
    let sql = "DROP TABLE a; DROP TABLE b;  -- pgsafe:ignore drop-table  only b is safe";
    let fs = lint_sql(sql, &LintOptions::default()).unwrap();
    let drops: Vec<_> = fs.iter().filter(|f| f.rule_id == "drop-table").collect();
    assert_eq!(drops.len(), 2);
    assert!(!drops[0].is_suppressed(), "first statement still gates");
    assert!(
        drops[1].is_suppressed(),
        "the rightmost statement (the one the comment trails) is suppressed"
    );
}
#[test]
fn hygiene_diagnostic_severities_are_locked() {
    use pgsafe::Severity;
    let sev = |sql: &str, id: &str| -> Severity {
        lint_sql(sql, &LintOptions::default())
            .unwrap()
            .into_iter()
            .find(|f| f.rule_id == id)
            .unwrap_or_else(|| panic!("expected a {id} finding"))
            .severity
    };
    assert_eq!(
        sev(
            "-- pgsafe:disable drop-table\nDROP TABLE x;",
            "suppression-malformed"
        ),
        Severity::Error
    );
    assert_eq!(
        sev(
            "-- pgsafe:ignore drop-tabel typo\nDROP TABLE x;",
            "suppression-unknown-rule"
        ),
        Severity::Error
    );
    assert_eq!(
        sev(
            "-- pgsafe:ignore drop-table\nDROP TABLE x;",
            "suppression-missing-reason"
        ),
        Severity::Error
    );
    assert_eq!(
        sev(
            "-- pgsafe:ignore truncate  stale\nDELETE FROM x;",
            "suppression-unused"
        ),
        Severity::Warning
    );
}
