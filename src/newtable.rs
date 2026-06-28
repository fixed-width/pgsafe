//! New-table awareness: drop findings on operations against a table created
//! (empty) earlier in the same input, since those operations cannot lock,
//! rewrite, or scan an empty, not-yet-visible table. A table that is later
//! populated (INSERT / COPY ... FROM) is no longer treated as empty.

use std::collections::BTreeSet;

use pg_query::protobuf::{AlterTableType, ObjectType, RangeVar, RawStmt};
use pg_query::NodeEnum;

use crate::Finding;

/// `RangeVar.relpersistence` for a temporary relation (libpg_query's `RELPERSISTENCE_TEMP`).
const RELPERSISTENCE_TEMP: &str = "t";

/// A cross-statement table key. The default `public` schema is normalized to bare, so a table
/// written `t` in one statement and `public.t` in another correlates (`public` is the default
/// search_path schema); any other schema stays distinct (`app.t` is not `t`). This is what lets the
/// cross-statement rules (fk-without-covering-index, require-comment, require-columns, the new-table
/// exemption) match the same table across spellings.
pub(crate) fn qualified_key(schema: &str, relname: &str) -> String {
    if schema.is_empty() || schema == "public" {
        relname.to_string()
    } else {
        format!("{schema}.{relname}")
    }
}

/// The cross-statement key of a `RangeVar` (see [`qualified_key`]).
pub(crate) fn rangevar_key(rv: &RangeVar) -> String {
    qualified_key(&rv.schemaname, &rv.relname)
}

/// The `RangeVar` of a `CREATE TABLE` the schema-design and policy lints care about: a persistent,
/// non-partition-child table. `None` for any other node, a `PARTITION OF` child (it inherits the
/// parent's columns), or a temporary table.
pub(crate) fn lintable_create_relation(node: &NodeEnum) -> Option<&RangeVar> {
    let NodeEnum::CreateStmt(c) = node else {
        return None;
    };
    if c.partbound.is_some() {
        return None;
    }
    let rv = c.relation.as_ref()?;
    if rv.relpersistence == RELPERSISTENCE_TEMP {
        return None;
    }
    Some(rv)
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

/// For an `ALTER TABLE … ATTACH PARTITION child …`, the child being attached — whose emptiness, not
/// the parent's, determines whether the validation scan is trivial. `None` for any other node.
fn attach_partition_child(node: &NodeEnum) -> Option<String> {
    let NodeEnum::AlterTableStmt(a) = node else {
        return None;
    };
    a.cmds.iter().find_map(|n| {
        let NodeEnum::AlterTableCmd(cmd) = n.node.as_ref()? else {
            return None;
        };
        if AlterTableType::try_from(cmd.subtype) != Ok(AlterTableType::AtAttachPartition) {
            return None;
        }
        match cmd.def.as_ref()?.node.as_ref()? {
            NodeEnum::PartitionCmd(pc) => pc.name.as_ref().map(rangevar_key),
            _ => None,
        }
    })
}

/// The single table a `DROP TABLE` targets, or `None` for a non-table drop or a multi-table
/// `DROP TABLE a, b` (conservatively not exempted).
fn drop_table_key(node: &NodeEnum) -> Option<String> {
    let NodeEnum::DropStmt(d) = node else {
        return None;
    };
    if ObjectType::try_from(d.remove_type) != Ok(ObjectType::ObjectTable) || d.objects.len() != 1 {
        return None;
    }
    // A drop object is a `List` of the name parts (`["t"]` or `["schema", "t"]`); build the same key
    // shape as `rangevar_key` (with the default `public` schema normalized to bare).
    let NodeEnum::List(list) = d.objects[0].node.as_ref()? else {
        return None;
    };
    let parts: Vec<&str> = list
        .items
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(NodeEnum::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [relname] => Some(qualified_key("", relname)),
        [schema, relname] => Some(qualified_key(schema, relname)),
        _ => None,
    }
}

/// The single table a `TRUNCATE` targets, or `None` for a multi-table truncate.
fn truncate_table_key(node: &NodeEnum) -> Option<String> {
    let NodeEnum::TruncateStmt(t) = node else {
        return None;
    };
    if t.relations.len() != 1 {
        return None;
    }
    match t.relations[0].node.as_ref()? {
        NodeEnum::RangeVar(rv) => Some(rangevar_key(rv)),
        _ => None,
    }
}

/// Table an `ALTER TABLE`, `CREATE INDEX`, `CREATE TRIGGER`, `RENAME`, single-table `DROP TABLE`, or
/// single-table `TRUNCATE` operates on. For an `ALTER TABLE … ATTACH PARTITION`, the operated-on table
/// is the partition child.
fn target_table_key(node: &NodeEnum) -> Option<String> {
    if let Some(child) = attach_partition_child(node) {
        return Some(child);
    }
    match node {
        NodeEnum::AlterTableStmt(a) => a.relation.as_ref().map(rangevar_key),
        NodeEnum::IndexStmt(i) => i.relation.as_ref().map(rangevar_key),
        NodeEnum::CreateTrigStmt(t) => t.relation.as_ref().map(rangevar_key),
        NodeEnum::RenameStmt(r) => r.relation.as_ref().map(rangevar_key),
        NodeEnum::DropStmt(_) => drop_table_key(node),
        NodeEnum::TruncateStmt(_) => truncate_table_key(node),
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

    #[test]
    fn qualified_key_normalizes_public_to_bare() {
        assert_eq!(qualified_key("", "t"), "t");
        assert_eq!(qualified_key("public", "t"), "t");
        assert_eq!(qualified_key("app", "t"), "app.t");
    }

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
            target_table_key(&first_node(
                "ALTER TABLE parent ATTACH PARTITION child FOR VALUES FROM (0) TO (100)"
            ))
            .as_deref(),
            Some("child")
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
