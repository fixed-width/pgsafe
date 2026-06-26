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

// ── drop-index-non-concurrent ───────────────────────────────────────────────

#[test]
fn drop_index_non_concurrent() {
    assert!(fires("DROP INDEX my_idx", "drop-index-non-concurrent"));
    assert!(!fires(
        "DROP INDEX CONCURRENTLY my_idx",
        "drop-index-non-concurrent"
    ));
}

// ── drop-table ──────────────────────────────────────────────────────────────

#[test]
fn drop_table() {
    assert!(fires("DROP TABLE t", "drop-table"));
    assert!(!fires("DROP INDEX i", "drop-table"));
}

// ── drop-column ─────────────────────────────────────────────────────────────

#[test]
fn drop_column() {
    assert!(fires("ALTER TABLE t DROP COLUMN c", "drop-column"));
    assert!(!fires("ALTER TABLE t ADD COLUMN c int", "drop-column"));
}

// ── truncate ─────────────────────────────────────────────────────────────────

#[test]
fn truncate() {
    assert!(fires("TRUNCATE t", "truncate"));
    assert!(!fires("DELETE FROM t", "truncate"));
}

// ── vacuum-full-cluster ───────────────────────────────────────────────────────

#[test]
fn vacuum_full_cluster() {
    assert!(fires("VACUUM FULL t", "vacuum-full-cluster"));
    assert!(fires("VACUUM (FULL) t", "vacuum-full-cluster"));
    assert!(fires("CLUSTER t USING idx", "vacuum-full-cluster"));
    assert!(!fires("VACUUM t", "vacuum-full-cluster"));
    assert!(!fires("VACUUM (ANALYZE) t", "vacuum-full-cluster"));
    assert!(!fires("ANALYZE t", "vacuum-full-cluster"));
    // explicit false/off/0 must NOT fire (false positives fixed)
    assert!(!fires("VACUUM (FULL false) t", "vacuum-full-cluster"));
    assert!(!fires("VACUUM (FULL off) t", "vacuum-full-cluster"));
    assert!(!fires("VACUUM (FULL 0) t", "vacuum-full-cluster"));
    // explicit true still fires
    assert!(fires("VACUUM (FULL true) t", "vacuum-full-cluster"));
}

// ── reindex-non-concurrent ────────────────────────────────────────────────────

#[test]
fn reindex_non_concurrent() {
    assert!(fires("REINDEX INDEX my_idx", "reindex-non-concurrent"));
    assert!(!fires(
        "REINDEX INDEX CONCURRENTLY my_idx",
        "reindex-non-concurrent"
    ));
    // explicit false must fire (false negative fixed: CONCURRENTLY false means NOT concurrent)
    assert!(fires(
        "REINDEX (CONCURRENTLY false) INDEX my_idx",
        "reindex-non-concurrent"
    ));
    // explicit true must NOT fire
    assert!(!fires(
        "REINDEX (CONCURRENTLY true) INDEX my_idx",
        "reindex-non-concurrent"
    ));
}

// ── add-unique-constraint ─────────────────────────────────────────────────────

#[test]
fn add_unique_constraint() {
    assert!(fires(
        "ALTER TABLE t ADD CONSTRAINT u UNIQUE (a)",
        "add-unique-constraint"
    ));
    assert!(!fires(
        "ALTER TABLE t ADD CONSTRAINT u UNIQUE USING INDEX existing_idx",
        "add-unique-constraint"
    ));
}

// ── add-primary-key-without-index ─────────────────────────────────────────────

#[test]
fn add_primary_key_without_index() {
    assert!(fires(
        "ALTER TABLE t ADD CONSTRAINT pk PRIMARY KEY (id)",
        "add-primary-key-without-index"
    ));
    assert!(fires(
        "ALTER TABLE t ADD COLUMN id int PRIMARY KEY",
        "add-primary-key-without-index"
    ));
    assert!(!fires(
        "ALTER TABLE t ADD CONSTRAINT pk PRIMARY KEY USING INDEX existing_idx",
        "add-primary-key-without-index"
    ));
}

// ── add-column-not-null-no-default ────────────────────────────────────────────

#[test]
fn add_column_not_null_no_default() {
    assert!(fires(
        "ALTER TABLE t ADD COLUMN c int NOT NULL",
        "add-column-not-null-no-default"
    ));
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN c int",
        "add-column-not-null-no-default"
    ));
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN c int NOT NULL DEFAULT 0",
        "add-column-not-null-no-default"
    ));
}
