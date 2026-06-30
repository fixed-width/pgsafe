//! End-to-end auto-fix proofs over the PUBLIC contract: a consumer reads
//! `Finding.fix` (the serialized shape) and splices the edits. Asserts each
//! pilot fix clears its finding and still parses.

use pgsafe::{lint_sql, Finding, Fix, LintOptions};

/// Splice a fix's edits over `sql`, exactly as a JSON consumer would.
fn apply(sql: &str, fix: &Fix) -> String {
    let mut out = sql.to_string();
    let mut edits = fix.edits.clone();
    edits.sort_by_key(|e| std::cmp::Reverse(e.start));
    for e in edits {
        out.replace_range(e.start as usize..e.end as usize, &e.replacement);
    }
    out
}

fn fix_for(sql: &str, rule: &str) -> (Vec<Finding>, Option<Fix>) {
    let fs = lint_sql(sql, &LintOptions::default()).unwrap();
    let fix = fs
        .iter()
        .find(|f| f.rule_id == rule)
        .and_then(|f| f.fix.clone());
    (fs, fix)
}

fn assert_clears(sql: &str, rule: &str) {
    let (_, fix) = fix_for(sql, rule);
    let fix = fix.unwrap_or_else(|| panic!("{rule} produced no fix for: {sql}"));
    let fixed = apply(sql, &fix);
    // Still parses (lint_sql errors on a parse failure).
    let after = lint_sql(&fixed, &LintOptions::default())
        .unwrap_or_else(|e| panic!("fixed SQL did not parse: {fixed}\n{e}"));
    // The finding is gone.
    assert!(
        after.iter().all(|f| f.rule_id != rule),
        "fix did not clear {rule}: {fixed}"
    );
}

#[test]
fn add_index_fix_clears_and_parses() {
    assert_clears("CREATE INDEX idx ON t (col);", "add-index-non-concurrent");
}

#[test]
fn require_timeout_fix_clears_and_parses() {
    assert_clears("ALTER TABLE t ADD COLUMN c int;", "require-timeout");
}

#[test]
fn add_index_fix_clears_unique_index() {
    assert_clears(
        "CREATE UNIQUE INDEX idx ON t (col);",
        "add-index-non-concurrent",
    );
}

#[test]
fn require_timeout_first_finding_has_fix_subsequent_do_not() {
    // Both statements take a blocking lock without a timeout — both flag require-timeout.
    // Only the first finding should carry the fix (one migration-level prologue).
    let sql = "CREATE INDEX i ON t (a);\nDROP TABLE other;";
    let fs = lint_sql(sql, &LintOptions::default()).unwrap();
    let timeout_findings: Vec<_> = fs
        .iter()
        .filter(|f| f.rule_id == "require-timeout")
        .collect();
    assert!(
        timeout_findings.len() >= 2,
        "expected ≥2 require-timeout findings, got {}",
        timeout_findings.len()
    );
    assert!(
        timeout_findings[0].fix.is_some(),
        "first require-timeout finding must carry the fix"
    );
    assert!(
        timeout_findings[1].fix.is_none(),
        "subsequent require-timeout findings must not carry a fix"
    );
}

#[test]
fn require_timeout_single_fix_clears_all_findings() {
    // Applying the fix from the first finding should clear ALL require-timeout findings.
    let sql = "CREATE INDEX i ON t (a);\nDROP TABLE other;";
    let fs = lint_sql(sql, &LintOptions::default()).unwrap();
    let fix = fs
        .iter()
        .find(|f| f.rule_id == "require-timeout")
        .and_then(|f| f.fix.clone())
        .expect("require-timeout fix present on first finding");
    let fixed = apply(sql, &fix);
    let after = lint_sql(&fixed, &LintOptions::default())
        .unwrap_or_else(|e| panic!("fixed SQL did not parse: {e}"));
    assert!(
        after.iter().all(|f| f.rule_id != "require-timeout"),
        "single fix should clear all require-timeout findings; fixed SQL:\n{fixed}"
    );
}

#[test]
fn require_timeout_fix_inserts_before_first_flagged_statement() {
    let sql = "SELECT 1;\nCREATE INDEX i ON t (a);";
    let (_, fix) = fix_for(sql, "require-timeout");
    let fix = fix.expect("require-timeout fix present");
    let first_edit = fix
        .edits
        .first()
        .expect("timeout fix has at least one edit");
    // The prologue must go before the flagged CREATE INDEX, not at byte 0.
    assert!(
        first_edit.start > 0,
        "prologue should precede the flagged statement, not byte 0"
    );
    // And it should land exactly at the start of the flagged statement.
    let at = first_edit.start as usize;
    assert!(
        sql[at..].starts_with("CREATE INDEX"),
        "prologue anchored at: {:?}",
        &sql[at..]
    );
}
