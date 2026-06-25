//! Table-driven rule coverage: one test function per rule, asserting both
//! the "fires" and "does not fire" polarities. New rules extend this file.

use pgsafe::lint_sql;

fn fires(sql: &str, rule_id: &str) -> bool {
    lint_sql(sql).unwrap().iter().any(|f| f.rule_id == rule_id)
}

// ── non-concurrent-index ────────────────────────────────────────────────────

#[test]
fn non_concurrent_index_fires() {
    assert!(fires("CREATE INDEX i ON t (x)", "non-concurrent-index"));
    assert!(fires(
        "CREATE UNIQUE INDEX i ON t (x)",
        "non-concurrent-index"
    ));
}

#[test]
fn non_concurrent_index_silent() {
    assert!(!fires(
        "CREATE INDEX CONCURRENTLY i ON t (x)",
        "non-concurrent-index"
    ));
    assert!(!fires(
        "CREATE UNIQUE INDEX CONCURRENTLY i ON t (x)",
        "non-concurrent-index"
    ));
}

// ── add-fk-without-not-valid ────────────────────────────────────────────────

#[test]
fn add_fk_without_not_valid_fires() {
    assert!(fires(
        "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id)",
        "add-fk-without-not-valid"
    ));
}

#[test]
fn add_fk_without_not_valid_silent() {
    // NOT VALID suppresses the rule
    assert!(!fires(
        "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id) NOT VALID",
        "add-fk-without-not-valid"
    ));
    // Non-FK constraints are not covered by this rule
    assert!(!fires(
        "ALTER TABLE t ADD CONSTRAINT pk PRIMARY KEY (id)",
        "add-fk-without-not-valid"
    ));
    assert!(!fires(
        "ALTER TABLE t ADD CONSTRAINT uq UNIQUE (email)",
        "add-fk-without-not-valid"
    ));
}

// ── add-check-without-not-valid ─────────────────────────────────────────────

#[test]
fn add_check_without_not_valid_fires() {
    assert!(fires(
        "ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0)",
        "add-check-without-not-valid"
    ));
}

#[test]
fn add_check_without_not_valid_silent() {
    // NOT VALID suppresses the rule
    assert!(!fires(
        "ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0) NOT VALID",
        "add-check-without-not-valid"
    ));
    // PRIMARY KEY is not a check constraint
    assert!(!fires(
        "ALTER TABLE t ADD CONSTRAINT pk PRIMARY KEY (id)",
        "add-check-without-not-valid"
    ));
}

// ── set-not-null ────────────────────────────────────────────────────────────

#[test]
fn set_not_null_fires() {
    assert!(fires(
        "ALTER TABLE t ALTER COLUMN a SET NOT NULL",
        "set-not-null"
    ));
}

#[test]
fn set_not_null_silent() {
    assert!(!fires(
        "ALTER TABLE t ALTER COLUMN a DROP NOT NULL",
        "set-not-null"
    ));
}

// ── alter-column-type ───────────────────────────────────────────────────────

#[test]
fn alter_column_type_fires() {
    assert!(fires(
        "ALTER TABLE t ALTER COLUMN a TYPE bigint",
        "alter-column-type"
    ));
}

#[test]
fn alter_column_type_silent() {
    assert!(!fires(
        "ALTER TABLE t ALTER COLUMN a SET DEFAULT 0",
        "alter-column-type"
    ));
}

// ── rename ──────────────────────────────────────────────────────────────────

#[test]
fn rename_fires() {
    assert!(fires("ALTER TABLE t RENAME TO t2", "rename"));
    assert!(fires("ALTER TABLE t RENAME COLUMN a TO b", "rename"));
    assert!(fires("ALTER INDEX i RENAME TO i2", "rename"));
    assert!(fires("ALTER TABLE t RENAME CONSTRAINT c TO c2", "rename"));
    assert!(fires("ALTER VIEW v RENAME TO v2", "rename"));
    assert!(fires("ALTER SEQUENCE s RENAME TO s2", "rename"));
}

#[test]
fn rename_silent_for_trigger() {
    // ALTER TRIGGER is not in the covered set (falls to `_ => return`).
    assert!(!fires("ALTER TRIGGER trg ON t RENAME TO trg2", "rename"));
}
