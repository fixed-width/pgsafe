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

// ---------------------------------------------------------------------------
// Regression: require-timeout prologue must land ABOVE leading directive comments
// ---------------------------------------------------------------------------

#[test]
fn require_timeout_prologue_lands_above_leading_directive() {
    // Before the fix: `timeout_fix` anchored at `geoms[first].start` — the first
    // TOKEN after skipping leading comments. When the statement had a leading
    // `-- pgsafe:ignore` directive, the prologue was inserted BETWEEN the directive
    // and the statement. That broke suppression (the directive no longer attached to
    // the now-displaced statement) and raised a spurious `suppression-unused`.
    //
    // After the fix: `timeout_fix` anchors at `geoms[first].prologue_anchor` — the
    // start of the statement's contiguous own-line comment block — so the prologue
    // goes ABOVE the entire comment block.
    let sql = "-- pgsafe:ignore add-index-non-concurrent  built in a maintenance window\n\
               CREATE INDEX idx_users_email ON users (email);";
    let fs = lint_sql(sql, &LintOptions::default()).unwrap();
    let rt_fix = fs
        .iter()
        .find(|f| f.rule_id == "require-timeout")
        .and_then(|f| f.fix.clone())
        .expect("require-timeout finding must carry a fix");
    let fixed = apply(sql, &rt_fix);
    let after = lint_sql(&fixed, &LintOptions::default())
        .unwrap_or_else(|e| panic!("fixed SQL did not parse:\n{fixed}\n{e}"));
    assert!(
        after.iter().all(|f| f.rule_id != "require-timeout"),
        "require-timeout must be cleared after the fix; fixed SQL:\n{fixed}"
    );
    assert!(
        after.iter().all(|f| f.rule_id != "suppression-unused"),
        "suppression-unused must not fire; the directive must still attach to the CREATE INDEX; \
         fixed SQL:\n{fixed}"
    );
    let ainc = after
        .iter()
        .find(|f| f.rule_id == "add-index-non-concurrent")
        .expect("add-index-non-concurrent must still be present (suppressed)");
    assert!(
        ainc.is_suppressed(),
        "add-index-non-concurrent must remain suppressed; fixed SQL:\n{fixed}"
    );
}

// ---------------------------------------------------------------------------
// Regression: multi-statement — timeout prologue must land ABOVE a leading
// directive on a non-first flagged statement
// ---------------------------------------------------------------------------

/// The first flagged statement is not statement 0.  Statement 0 (`CREATE TABLE`) is
/// non-blocking; statement 1 (`CREATE INDEX`) takes a blocking lock and has an
/// own-line `-- pgsafe:ignore add-index-non-concurrent` directive directly above it.
/// A trailing `-- pgsafe:ignore prefer-jsonb` rides on statement 0's line.
///
/// With the old `raw_start` anchor the prologue was inserted BETWEEN the own-line
/// directive and `CREATE INDEX` (pg_query doesn't attribute inter-statement comment
/// lines to the later statement), breaking suppression and raising `suppression-unused`.
/// With the new backward-line-walk anchor the prologue must land ABOVE the directive.
#[test]
fn require_timeout_prologue_lands_above_leading_directive_non_first_stmt() {
    let sql = "CREATE TABLE t (data json); -- pgsafe:ignore prefer-jsonb  ok\n\
               -- pgsafe:ignore add-index-non-concurrent  reason\n\
               CREATE INDEX i ON existing (col);";
    let fs = lint_sql(sql, &LintOptions::default()).unwrap();
    let rt_fix = fs
        .iter()
        .find(|f| f.rule_id == "require-timeout")
        .and_then(|f| f.fix.clone())
        .expect("require-timeout finding must carry a fix");
    let fixed = apply(sql, &rt_fix);
    let after = lint_sql(&fixed, &LintOptions::default())
        .unwrap_or_else(|e| panic!("fixed SQL did not parse:\n{fixed}\n{e}"));
    assert!(
        after.iter().all(|f| f.rule_id != "require-timeout"),
        "require-timeout must be cleared after the fix; fixed SQL:\n{fixed}"
    );
    assert!(
        after.iter().all(|f| f.rule_id != "suppression-unused"),
        "suppression-unused must not fire; fixed SQL:\n{fixed}"
    );
    let ainc = after
        .iter()
        .find(|f| f.rule_id == "add-index-non-concurrent")
        .expect("add-index-non-concurrent must still be present (suppressed)");
    assert!(
        ainc.is_suppressed(),
        "add-index-non-concurrent must remain suppressed; fixed SQL:\n{fixed}"
    );
    let pjsonb = after
        .iter()
        .find(|f| f.rule_id == "prefer-jsonb")
        .expect("prefer-jsonb must still be present (suppressed)");
    assert!(
        pjsonb.is_suppressed(),
        "prefer-jsonb must remain suppressed; fixed SQL:\n{fixed}"
    );
}

/// Same hazard as above, but the leading directive is a MULTI-LINE block comment.
/// The anchor walk must climb over the block comment's continuation/closing line
/// (`   reason */`), not stop below it — otherwise the prologue lands between `*/`
/// and `CREATE INDEX`, re-firing the suppressed rule and raising suppression-unused.
#[test]
fn require_timeout_prologue_lands_above_multiline_block_comment_directive() {
    let sql = "CREATE TABLE t (data json); -- pgsafe:ignore prefer-jsonb  ok\n\
               /* pgsafe:ignore add-index-non-concurrent\n\
               \x20  reason */\n\
               CREATE INDEX i ON existing (col);";
    let fs = lint_sql(sql, &LintOptions::default()).unwrap();
    let rt_fix = fs
        .iter()
        .find(|f| f.rule_id == "require-timeout")
        .and_then(|f| f.fix.clone())
        .expect("require-timeout finding must carry a fix");
    let fixed = apply(sql, &rt_fix);
    let after = lint_sql(&fixed, &LintOptions::default())
        .unwrap_or_else(|e| panic!("fixed SQL did not parse:\n{fixed}\n{e}"));
    assert!(
        after.iter().all(|f| f.rule_id != "require-timeout"),
        "require-timeout must be cleared after the fix; fixed SQL:\n{fixed}"
    );
    assert!(
        after.iter().all(|f| f.rule_id != "suppression-unused"),
        "suppression-unused must not fire; fixed SQL:\n{fixed}"
    );
    let ainc = after
        .iter()
        .find(|f| f.rule_id == "add-index-non-concurrent")
        .expect("add-index-non-concurrent must still be present (suppressed)");
    assert!(
        ainc.is_suppressed(),
        "add-index-non-concurrent must remain suppressed; fixed SQL:\n{fixed}"
    );
}

// ---------------------------------------------------------------------------
// Plan 2 producer integration tests (default LintOptions)
// ---------------------------------------------------------------------------
//
// `require-if-exists` (opt-in flag) and `forbidden-column-type` (requires a
// configured type map) are not reachable under default `LintOptions`, so they
// are NOT covered here. Their fix producers are exercised in the respective
// rule unit tests (Tasks 4 and 6 of Plan 2).

#[test]
fn drop_index_fix_clears() {
    assert_clears("DROP INDEX idx;", "drop-index-non-concurrent");
}

#[test]
fn reindex_fix_clears() {
    assert_clears("REINDEX INDEX idx;", "reindex-non-concurrent");
}

#[test]
fn detach_partition_fix_clears() {
    assert_clears(
        "ALTER TABLE p DETACH PARTITION p1;",
        "detach-partition-non-concurrent",
    );
}

#[test]
fn add_check_fix_clears() {
    assert_clears(
        "ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0);",
        "add-check-without-not-valid",
    );
}

#[test]
fn add_fk_fix_clears() {
    assert_clears(
        "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id);",
        "add-fk-without-not-valid",
    );
}

#[test]
fn prefer_jsonb_fix_clears() {
    assert_clears("CREATE TABLE t (data json);", "prefer-jsonb");
}

#[test]
fn prefer_bigint_fix_clears() {
    assert_clears(
        "CREATE TABLE t (id integer PRIMARY KEY);",
        "prefer-bigint-primary-key",
    );
}

/// A StatementBodyEnd fix on a statement with a trailing comment and NO terminating semicolon must
/// splice before the comment, not inside it — and must converge (regression for the trailing-comment
/// corruption where ` NOT VALID` landed inside the comment and the fixpoint looped).
#[test]
fn body_end_fix_lands_before_trailing_line_comment() {
    let sql = "ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0) -- keep me";
    let (_, fix) = fix_for(sql, "add-check-without-not-valid");
    let fixed = apply(sql, &fix.expect("fix present"));
    assert_eq!(
        fixed,
        "ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0) NOT VALID -- keep me"
    );
    assert_clears(sql, "add-check-without-not-valid");
}

#[test]
fn body_end_fix_lands_before_trailing_block_comment() {
    let sql = "ALTER DOMAIN d ADD CONSTRAINT c CHECK (VALUE > 0) /* keep */";
    let (_, fix) = fix_for(sql, "add-domain-constraint-without-not-valid");
    let fixed = apply(sql, &fix.expect("fix present"));
    assert_eq!(
        fixed,
        "ALTER DOMAIN d ADD CONSTRAINT c CHECK (VALUE > 0) NOT VALID /* keep */"
    );
    assert_clears(sql, "add-domain-constraint-without-not-valid");
}
