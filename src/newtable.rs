//! New-table awareness: drop findings on operations against a table created
//! (empty) earlier in the same input, since those operations cannot lock,
//! rewrite, or scan an empty, not-yet-visible table. A table that is later
//! populated (INSERT / COPY ... FROM) is no longer treated as empty.

use std::collections::BTreeSet;

use pg_query::protobuf::{RangeVar, RawStmt};
use pg_query::NodeEnum;

use crate::Finding;

/// `schemaname.relname`, or just `relname` when unqualified.
fn rangevar_key(rv: &RangeVar) -> String {
    if rv.schemaname.is_empty() {
        rv.relname.clone()
    } else {
        format!("{}.{}", rv.schemaname, rv.relname)
    }
}

/// Table created by a bare `CREATE TABLE` (`CreateStmt`). `CREATE TABLE AS`
/// (`CreateTableAsStmt`) is intentionally excluded — it is populated.
fn created_table_key(node: &NodeEnum) -> Option<String> {
    match node {
        NodeEnum::CreateStmt(c) => c.relation.as_ref().map(rangevar_key),
        _ => None,
    }
}

/// Table an `INSERT`, `MERGE INTO`, or `COPY ... FROM` populates.
fn populated_table_key(node: &NodeEnum) -> Option<String> {
    match node {
        NodeEnum::InsertStmt(i) => i.relation.as_ref().map(rangevar_key),
        NodeEnum::MergeStmt(m) => m.relation.as_ref().map(rangevar_key),
        NodeEnum::CopyStmt(c) if c.is_from => c.relation.as_ref().map(rangevar_key),
        // A top-level writable CTE (`WITH x AS (INSERT ...) ...`) that populates a tracked
        // table is not detected here; rare, accepted as a documented limitation.
        _ => None,
    }
}

/// Table an `ALTER TABLE` / `CREATE INDEX` operates on.
fn target_table_key(node: &NodeEnum) -> Option<String> {
    match node {
        NodeEnum::AlterTableStmt(a) => a.relation.as_ref().map(rangevar_key),
        NodeEnum::IndexStmt(i) => i.relation.as_ref().map(rangevar_key),
        NodeEnum::CreateTrigStmt(t) => t.relation.as_ref().map(rangevar_key),
        _ => None,
    }
}

/// Drop findings on statements that target a table created empty earlier in the
/// same input and not since populated. Returns the kept findings and the set of
/// dropped statement indices (so inline-suppression can avoid reporting a now-redundant
/// directive on such a statement as unused).
pub(crate) fn drop_new_table_findings(
    stmts: &[RawStmt],
    findings: Vec<Finding>,
) -> (Vec<Finding>, BTreeSet<usize>) {
    let mut empty: BTreeSet<String> = BTreeSet::new();
    let mut dropped: BTreeSet<usize> = BTreeSet::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        if let Some(key) = target_table_key(node) {
            if empty.contains(&key) {
                dropped.insert(i);
            }
        }
        if let Some(key) = created_table_key(node) {
            empty.insert(key);
        } else if let Some(key) = populated_table_key(node) {
            empty.remove(&key);
        }
    }
    if dropped.is_empty() {
        return (findings, dropped);
    }
    let kept = findings
        .into_iter()
        .filter(|f| !dropped.contains(&f.statement_index))
        .collect();
    (kept, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_node(sql: &str) -> NodeEnum {
        pg_query::parse(sql).unwrap().protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .as_ref()
            .unwrap()
            .clone()
    }

    #[test]
    fn key_extraction() {
        assert_eq!(
            created_table_key(&first_node("CREATE TABLE foo (id int)")).as_deref(),
            Some("foo")
        );
        assert_eq!(
            created_table_key(&first_node("CREATE TABLE s.foo (id int)")).as_deref(),
            Some("s.foo")
        );
        assert_eq!(
            created_table_key(&first_node("CREATE TABLE foo AS SELECT 1")),
            None
        );
        assert_eq!(
            target_table_key(&first_node("ALTER TABLE foo ADD COLUMN c int")).as_deref(),
            Some("foo")
        );
        assert_eq!(
            target_table_key(&first_node("CREATE INDEX i ON foo (x)")).as_deref(),
            Some("foo")
        );
        assert_eq!(
            target_table_key(&first_node(
                "CREATE TRIGGER trg AFTER INSERT ON foo FOR EACH ROW EXECUTE FUNCTION f()"
            ))
            .as_deref(),
            Some("foo")
        );
        assert_eq!(
            populated_table_key(&first_node("INSERT INTO foo VALUES (1)")).as_deref(),
            Some("foo")
        );
        assert_eq!(
            populated_table_key(&first_node("COPY foo FROM '/tmp/x'")).as_deref(),
            Some("foo")
        );
        assert_eq!(
            populated_table_key(&first_node("COPY foo TO '/tmp/x'")),
            None
        );
        assert_eq!(
            populated_table_key(&first_node(
                "MERGE INTO foo USING src ON foo.id = src.id WHEN NOT MATCHED THEN INSERT VALUES (src.id)"
            ))
            .as_deref(),
            Some("foo")
        );
    }
}
