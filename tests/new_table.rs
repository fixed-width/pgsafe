use pgsafe::{lint_sql, LintOptions};

fn fires(sql: &str, rule_id: &str) -> bool {
    lint_sql(sql, &LintOptions::default())
        .unwrap()
        .iter()
        .any(|f| f.rule_id == rule_id)
}

#[test]
fn merge_populated_new_table_still_fires() {
    assert!(fires(
        "CREATE TABLE foo (id int); \
         MERGE INTO foo USING src ON foo.id = src.id WHEN NOT MATCHED THEN INSERT VALUES (src.id); \
         ALTER TABLE foo ADD CONSTRAINT u UNIQUE (id);",
        "add-unique-constraint"
    ));
}

#[test]
fn directive_on_dropped_new_table_op_is_not_unused() {
    // A redundant inline directive on a new-table-safe op must not flip the gate red.
    let fs = lint_sql(
        "CREATE TABLE foo (id int);\n\
         -- pgsafe:ignore add-unique-constraint  belt and suspenders\n\
         ALTER TABLE foo ADD CONSTRAINT u UNIQUE (id);",
        &LintOptions::default(),
    )
    .unwrap();
    assert!(
        !fs.iter().any(|f| f.rule_id == "suppression-unused"),
        "directive on a dropped new-table op must not be reported unused"
    );
    assert!(fs.is_empty(), "the run should be clean");
}

#[test]
fn empty_new_table_operations_are_dropped() {
    assert!(!fires(
        "CREATE TABLE foo (id int); ALTER TABLE foo ADD CONSTRAINT u UNIQUE (id);",
        "add-unique-constraint"
    ));
    assert!(!fires(
        "CREATE TABLE foo (id int); ALTER TABLE foo ADD COLUMN c uuid DEFAULT gen_random_uuid();",
        "add-column-volatile-default"
    ));
    assert!(!fires(
        "CREATE TABLE foo (id int); CREATE INDEX i ON foo (id);",
        "add-index-non-concurrent"
    ));
}

#[test]
fn drop_truncate_rename_on_new_empty_table_are_dropped() {
    // A table created empty in the same migration has no data and no traffic, so dropping, truncating,
    // or renaming it is safe — the findings must be dropped.
    assert!(!fires(
        "CREATE TABLE foo (id int); DROP TABLE foo;",
        "drop-table"
    ));
    assert!(!fires(
        "CREATE TABLE foo (id int); TRUNCATE foo;",
        "truncate"
    ));
    assert!(!fires(
        "CREATE TABLE foo (id int); ALTER TABLE foo RENAME TO bar;",
        "rename"
    ));
    assert!(!fires(
        "CREATE TABLE foo (id int, a int); ALTER TABLE foo RENAME COLUMN a TO b;",
        "rename"
    ));
}

#[test]
fn drop_truncate_rename_on_existing_or_populated_table_still_fire() {
    // No CREATE in the migration → the table is pre-existing → not exempt.
    assert!(fires("DROP TABLE foo;", "drop-table"));
    assert!(fires("TRUNCATE foo;", "truncate"));
    assert!(fires("ALTER TABLE foo RENAME TO bar;", "rename"));
    // Populated in the same migration → no longer empty → still fires.
    assert!(fires(
        "CREATE TABLE foo (id int); INSERT INTO foo VALUES (1); DROP TABLE foo;",
        "drop-table"
    ));
}

#[test]
fn populated_new_table_still_fires() {
    // INSERT populates → flagged
    assert!(fires(
        "CREATE TABLE foo (id int); INSERT INTO foo VALUES (1); ALTER TABLE foo ADD CONSTRAINT u UNIQUE (id);",
        "add-unique-constraint"
    ));
    // COPY ... FROM populates → flagged
    assert!(fires(
        "CREATE TABLE foo (id int); COPY foo FROM '/tmp/data.csv'; ALTER TABLE foo ADD CONSTRAINT u UNIQUE (id);",
        "add-unique-constraint"
    ));
    // CREATE TABLE AS is born populated → flagged
    assert!(fires(
        "CREATE TABLE foo AS SELECT 1 AS id; ALTER TABLE foo ADD CONSTRAINT u UNIQUE (id);",
        "add-unique-constraint"
    ));
}

#[test]
fn not_a_new_table_still_fires() {
    // bar not created in this input
    assert!(fires(
        "ALTER TABLE bar ADD CONSTRAINT u UNIQUE (id);",
        "add-unique-constraint"
    ));
    // different table than the one created
    assert!(fires(
        "CREATE TABLE foo (id int); ALTER TABLE other ADD CONSTRAINT u UNIQUE (id);",
        "add-unique-constraint"
    ));
    // schema-qualified mismatch — conservative, still fires
    assert!(fires(
        "CREATE TABLE s.foo (id int); ALTER TABLE foo ADD CONSTRAINT u UNIQUE (id);",
        "add-unique-constraint"
    ));
}

#[test]
fn alter_before_create_still_fires() {
    assert!(fires(
        "ALTER TABLE foo ADD CONSTRAINT u UNIQUE (id); CREATE TABLE foo (id int);",
        "add-unique-constraint"
    ));
}
