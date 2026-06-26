//! New-table awareness: drop findings on operations against a table created
//! (empty) earlier in the same input, since those operations cannot lock,
//! rewrite, or scan an empty, not-yet-visible table. A table that is later
//! populated (INSERT / COPY ... FROM) is no longer treated as empty.

use std::collections::HashSet;

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

/// Table an `INSERT` or `COPY ... FROM` populates.
fn populated_table_key(node: &NodeEnum) -> Option<String> {
    match node {
        NodeEnum::InsertStmt(i) => i.relation.as_ref().map(rangevar_key),
        NodeEnum::CopyStmt(c) if c.is_from => c.relation.as_ref().map(rangevar_key),
        _ => None,
    }
}

/// Table an `ALTER TABLE` / `CREATE INDEX` operates on.
fn target_table_key(node: &NodeEnum) -> Option<String> {
    match node {
        NodeEnum::AlterTableStmt(a) => a.relation.as_ref().map(rangevar_key),
        NodeEnum::IndexStmt(i) => i.relation.as_ref().map(rangevar_key),
        _ => None,
    }
}

/// Drop findings on statements that target a table created empty earlier in the
/// same input and not since populated.
pub(crate) fn drop_new_table_findings(stmts: &[RawStmt], findings: Vec<Finding>) -> Vec<Finding> {
    let mut empty: HashSet<String> = HashSet::new();
    let mut dropped: HashSet<usize> = HashSet::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        // Evaluate the target against the set BEFORE applying this statement's own effect.
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
        return findings;
    }
    findings
        .into_iter()
        .filter(|f| !dropped.contains(&f.statement_index))
        .collect()
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
    }
}
