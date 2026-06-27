//! Table-driven rule coverage: one test function per rule, asserting both
//! the "fires" and "does not fire" polarities. New rules extend this file.

use pgsafe::{lint_sql, LintOptions};

fn fires(sql: &str, rule_id: &str) -> bool {
    lint_sql(sql, &LintOptions::default())
        .unwrap()
        .iter()
        .any(|f| f.rule_id == rule_id)
}

// ── add-index-non-concurrent ────────────────────────────────────────────────

#[test]
fn add_index_non_concurrent_fires() {
    assert!(fires("CREATE INDEX i ON t (x)", "add-index-non-concurrent"));
    assert!(fires(
        "CREATE UNIQUE INDEX i ON t (x)",
        "add-index-non-concurrent"
    ));
}

#[test]
fn add_index_non_concurrent_silent() {
    assert!(!fires(
        "CREATE INDEX CONCURRENTLY i ON t (x)",
        "add-index-non-concurrent"
    ));
    assert!(!fires(
        "CREATE UNIQUE INDEX CONCURRENTLY i ON t (x)",
        "add-index-non-concurrent"
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
    // Table-level ADD CONSTRAINT UNIQUE fires
    assert!(fires(
        "ALTER TABLE t ADD CONSTRAINT u UNIQUE (a)",
        "add-unique-constraint"
    ));
    // Attaching a pre-built index is safe
    assert!(!fires(
        "ALTER TABLE t ADD CONSTRAINT u UNIQUE USING INDEX existing_idx",
        "add-unique-constraint"
    ));
    // Column-level inline UNIQUE also fires (builds the index under ACCESS EXCLUSIVE)
    assert!(fires(
        "ALTER TABLE t ADD COLUMN c int UNIQUE",
        "add-unique-constraint"
    ));
    // Column without UNIQUE constraint is safe
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN c int",
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

// ── add-column-serial ─────────────────────────────────────────────────────────

#[test]
fn add_column_serial_fires() {
    assert!(fires(
        "ALTER TABLE t ADD COLUMN id serial",
        "add-column-serial"
    ));
    assert!(fires(
        "ALTER TABLE t ADD COLUMN id bigserial",
        "add-column-serial"
    ));
    assert!(fires(
        "ALTER TABLE t ADD COLUMN id smallserial",
        "add-column-serial"
    ));
    assert!(fires(
        "ALTER TABLE t ADD COLUMN id serial8",
        "add-column-serial"
    ));
    assert!(fires(
        "ALTER TABLE t ADD COLUMN id serial2",
        "add-column-serial"
    ));
    assert!(fires(
        "ALTER TABLE t ADD COLUMN id serial4",
        "add-column-serial"
    ));
    // case-insensitive
    assert!(fires(
        "ALTER TABLE t ADD COLUMN id BIGSERIAL",
        "add-column-serial"
    ));
}

#[test]
fn add_column_serial_one_finding_per_column() {
    let hits = lint_sql(
        "ALTER TABLE t ADD COLUMN a serial, ADD COLUMN b bigserial",
        &LintOptions::default(),
    )
    .unwrap()
    .into_iter()
    .filter(|f| f.rule_id == "add-column-serial")
    .count();
    assert_eq!(hits, 2);
}

#[test]
fn add_column_serial_silent() {
    // plain integer types are fine
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN id bigint",
        "add-column-serial"
    ));
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN id int",
        "add-column-serial"
    ));
    // identity is a separate rule, not a serial type
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN id int GENERATED ALWAYS AS IDENTITY",
        "add-column-serial"
    ));
    // CREATE TABLE is a new/empty table — out of scope
    assert!(!fires(
        "CREATE TABLE t (id bigserial PRIMARY KEY)",
        "add-column-serial"
    ));
}

// ── add-column-volatile-default ───────────────────────────────────────────────

#[test]
fn add_column_volatile_default_fires() {
    assert!(fires(
        "ALTER TABLE t ADD COLUMN id uuid DEFAULT gen_random_uuid()",
        "add-column-volatile-default"
    ));
    assert!(fires(
        "ALTER TABLE t ADD COLUMN r double precision DEFAULT random()",
        "add-column-volatile-default"
    ));
    assert!(fires(
        "ALTER TABLE t ADD COLUMN ts timestamptz DEFAULT clock_timestamp()",
        "add-column-volatile-default"
    ));
    assert!(fires(
        "ALTER TABLE t ADD COLUMN n bigint DEFAULT nextval('s')",
        "add-column-volatile-default"
    ));
    assert!(fires(
        "ALTER TABLE t ADD COLUMN id uuid DEFAULT uuid_generate_v4()",
        "add-column-volatile-default"
    ));
    // nested inside an expression
    assert!(fires(
        "ALTER TABLE t ADD COLUMN k int DEFAULT floor(random() * 100)",
        "add-column-volatile-default"
    ));
    // wrapped in a cast
    assert!(fires(
        "ALTER TABLE t ADD COLUMN s text DEFAULT gen_random_uuid()::text",
        "add-column-volatile-default"
    ));
    // GREATEST/LEAST (MinMaxExpr)
    assert!(fires(
        "ALTER TABLE t ADD COLUMN k double precision DEFAULT greatest(random(), 0.1)",
        "add-column-volatile-default"
    ));
    // ARRAY[...] (AArrayExpr)
    assert!(fires(
        "ALTER TABLE t ADD COLUMN a uuid[] DEFAULT ARRAY[gen_random_uuid()]",
        "add-column-volatile-default"
    ));
    // COALESCE (CoalesceExpr)
    assert!(fires(
        "ALTER TABLE t ADD COLUMN n bigint DEFAULT coalesce(nextval('s'), 0)",
        "add-column-volatile-default"
    ));
    // CASE (CaseExpr/CaseWhen)
    assert!(fires(
        "ALTER TABLE t ADD COLUMN k double precision DEFAULT (CASE WHEN true THEN random() ELSE 0 END)",
        "add-column-volatile-default"
    ));
    // boolean op (BoolExpr)
    assert!(fires(
        "ALTER TABLE t ADD COLUMN b boolean DEFAULT (random() > 0.5 OR false)",
        "add-column-volatile-default"
    ));
    // schema-qualified name (last-element match)
    assert!(fires(
        "ALTER TABLE t ADD COLUMN r double precision DEFAULT pg_catalog.random()",
        "add-column-volatile-default"
    ));
}

#[test]
fn add_column_volatile_default_silent() {
    // stable functions are safe (evaluated once, no rewrite)
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN ts timestamptz DEFAULT now()",
        "add-column-volatile-default"
    ));
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN ts timestamptz DEFAULT current_timestamp",
        "add-column-volatile-default"
    ));
    // constant default is safe
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN c int DEFAULT 0",
        "add-column-volatile-default"
    ));
    // no default at all
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN c int",
        "add-column-volatile-default"
    ));
    // SET DEFAULT is metadata-only (not ADD COLUMN) — out of scope
    assert!(!fires(
        "ALTER TABLE t ALTER COLUMN c SET DEFAULT random()",
        "add-column-volatile-default"
    ));
    // CREATE TABLE is empty — out of scope (the common UUID-PK pattern)
    assert!(!fires(
        "CREATE TABLE t (id uuid DEFAULT gen_random_uuid())",
        "add-column-volatile-default"
    ));
}

// ── add-column-identity ───────────────────────────────────────────────────────

#[test]
fn add_column_identity_fires() {
    assert!(fires(
        "ALTER TABLE t ADD COLUMN id int GENERATED ALWAYS AS IDENTITY",
        "add-column-identity"
    ));
    assert!(fires(
        "ALTER TABLE t ADD COLUMN id int GENERATED BY DEFAULT AS IDENTITY",
        "add-column-identity"
    ));
}

#[test]
fn add_column_identity_one_finding_per_column() {
    // Only the identity column fires; the plain column does not, and there is no double-count.
    let hits = lint_sql(
        "ALTER TABLE t ADD COLUMN a int, ADD COLUMN b int GENERATED ALWAYS AS IDENTITY",
        &LintOptions::default(),
    )
    .unwrap()
    .into_iter()
    .filter(|f| f.rule_id == "add-column-identity")
    .count();
    assert_eq!(hits, 1);
}

#[test]
fn add_column_identity_silent() {
    // generated-stored is a different constraint (ConstrGenerated) — separate rule
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN c int GENERATED ALWAYS AS (a + b) STORED",
        "add-column-identity"
    ));
    // serial is a separate rule
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN id bigserial",
        "add-column-identity"
    ));
    // plain type
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN id int",
        "add-column-identity"
    ));
    // CREATE TABLE is a new/empty table — out of scope
    assert!(!fires(
        "CREATE TABLE t (id int GENERATED ALWAYS AS IDENTITY)",
        "add-column-identity"
    ));
}

// ── add-column-generated-stored ───────────────────────────────────────────────

#[test]
fn add_column_generated_stored_fires() {
    assert!(fires(
        "ALTER TABLE t ADD COLUMN c int GENERATED ALWAYS AS (a + b) STORED",
        "add-column-generated-stored"
    ));
}

#[test]
fn add_column_generated_stored_one_finding_per_column() {
    // Only the generated column fires; the plain column does not, and there is no double-count.
    let hits = lint_sql(
        "ALTER TABLE t ADD COLUMN x int, ADD COLUMN c int GENERATED ALWAYS AS (a + b) STORED",
        &LintOptions::default(),
    )
    .unwrap()
    .into_iter()
    .filter(|f| f.rule_id == "add-column-generated-stored")
    .count();
    assert_eq!(hits, 1);
}

#[test]
fn add_column_generated_stored_silent() {
    // identity is a different constraint (ConstrIdentity) — separate rule
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN id int GENERATED ALWAYS AS IDENTITY",
        "add-column-generated-stored"
    ));
    // serial is a separate rule
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN id bigserial",
        "add-column-generated-stored"
    ));
    // plain type
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN c int",
        "add-column-generated-stored"
    ));
    // CREATE TABLE is a new/empty table — out of scope
    assert!(!fires(
        "CREATE TABLE t (c int GENERATED ALWAYS AS (a + b) STORED)",
        "add-column-generated-stored"
    ));
}

// ── set-logged-unlogged ───────────────────────────────────────────────────────

#[test]
fn set_logged_unlogged_fires() {
    assert!(fires("ALTER TABLE t SET UNLOGGED", "set-logged-unlogged"));
    assert!(fires("ALTER TABLE t SET LOGGED", "set-logged-unlogged"));
}

#[test]
fn set_logged_unlogged_silent() {
    // a different SET (storage parameter), not LOGGED/UNLOGGED
    assert!(!fires(
        "ALTER TABLE t SET (fillfactor = 70)",
        "set-logged-unlogged"
    ));
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN c int",
        "set-logged-unlogged"
    ));
}

// ── refresh-matview-non-concurrent ────────────────────────────────────────────

#[test]
fn refresh_matview_non_concurrent_fires() {
    assert!(fires(
        "REFRESH MATERIALIZED VIEW mv",
        "refresh-matview-non-concurrent"
    ));
}

#[test]
fn refresh_matview_non_concurrent_silent() {
    assert!(!fires(
        "REFRESH MATERIALIZED VIEW CONCURRENTLY mv",
        "refresh-matview-non-concurrent"
    ));
    // WITH NO DATA just empties the matview and is fast
    assert!(!fires(
        "REFRESH MATERIALIZED VIEW mv WITH NO DATA",
        "refresh-matview-non-concurrent"
    ));
}

// ── add-exclusion-constraint ──────────────────────────────────────────────────

#[test]
fn add_exclusion_constraint_fires() {
    assert!(fires(
        "ALTER TABLE t ADD CONSTRAINT e EXCLUDE USING gist (c WITH &&)",
        "add-exclusion-constraint"
    ));
}

#[test]
fn add_exclusion_constraint_silent() {
    // UNIQUE / CHECK are their own rules, not exclusion
    assert!(!fires(
        "ALTER TABLE t ADD CONSTRAINT u UNIQUE (a)",
        "add-exclusion-constraint"
    ));
    assert!(!fires(
        "ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0)",
        "add-exclusion-constraint"
    ));
}

// ── prefer-bigint-primary-key ─────────────────────────────────────────────────

#[test]
fn prefer_bigint_primary_key_fires() {
    assert!(fires(
        "CREATE TABLE t (id serial PRIMARY KEY)",
        "prefer-bigint-primary-key"
    ));
}

#[test]
fn prefer_bigint_primary_key_silent() {
    assert!(!fires(
        "CREATE TABLE t (id bigint PRIMARY KEY)",
        "prefer-bigint-primary-key"
    ));
}

// ── prefer-jsonb ──────────────────────────────────────────────────────────────

#[test]
fn prefer_jsonb_fires() {
    assert!(fires("CREATE TABLE t (data json)", "prefer-jsonb"));
    assert!(fires("ALTER TABLE t ADD COLUMN data json", "prefer-jsonb"));
}

#[test]
fn prefer_jsonb_silent() {
    assert!(!fires("CREATE TABLE t (data jsonb)", "prefer-jsonb"));
    assert!(!fires(
        "ALTER TABLE t ADD COLUMN data jsonb",
        "prefer-jsonb"
    ));
}
