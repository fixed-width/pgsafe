//! New-table awareness: drop findings on operations against a table created
//! (empty) earlier in the same input, since those operations cannot lock,
//! rewrite, or scan an empty, not-yet-visible table. A table that is later
//! populated (INSERT / COPY ... FROM) is no longer treated as empty.

use std::collections::BTreeSet;

use pg_query::protobuf::{AlterTableType, ConstrType, ObjectType, RangeVar, RawStmt};
use pg_query::NodeEnum;

use crate::{Finding, Severity};

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

/// For an `ALTER TABLE T ADD CONSTRAINT … CHECK (…)`, returns `(key of T, immediately_valid)`.
/// `immediately_valid` is `false` for `… NOT VALID` (the constraint is recorded but not enforced
/// until a later `VALIDATE CONSTRAINT`), mirroring `Constraint.skip_validation`. `None` for any
/// other node, a non-`ALTER TABLE`, or a non-CHECK constraint.
fn added_check_constraint(node: &NodeEnum) -> Option<(String, bool)> {
    let NodeEnum::AlterTableStmt(a) = node else {
        return None;
    };
    let key = a.relation.as_ref().map(rangevar_key)?;
    a.cmds.iter().find_map(|n| {
        let NodeEnum::AlterTableCmd(cmd) = n.node.as_ref()? else {
            return None;
        };
        if AlterTableType::try_from(cmd.subtype) != Ok(AlterTableType::AtAddConstraint) {
            return None;
        }
        let NodeEnum::Constraint(con) = cmd.def.as_ref()?.node.as_ref()? else {
            return None;
        };
        if ConstrType::try_from(con.contype) != Ok(ConstrType::ConstrCheck) {
            return None;
        }
        Some((key.clone(), !con.skip_validation))
    })
}

/// For an `ALTER TABLE T VALIDATE CONSTRAINT …`, the key of table `T`. `None` for any other node.
fn validated_constraint_table(node: &NodeEnum) -> Option<String> {
    let NodeEnum::AlterTableStmt(a) = node else {
        return None;
    };
    let key = a.relation.as_ref().map(rangevar_key)?;
    a.cmds.iter().find_map(|n| {
        let NodeEnum::AlterTableCmd(cmd) = n.node.as_ref()? else {
            return None;
        };
        (AlterTableType::try_from(cmd.subtype) == Ok(AlterTableType::AtValidateConstraint))
            .then(|| key.clone())
    })
}

/// Statement indices whose `attach-partition` finding should be escalated from `Warning` to
/// `Error`. A statement qualifies when it is an `ATTACH PARTITION` whose child was **not** created
/// earlier in this same migration **and** has **no** CHECK constraint prepared on it earlier in the
/// migration — either a plain `ADD … CHECK`, or an `ADD … CHECK … NOT VALID` later completed by a
/// `VALIDATE CONSTRAINT`. Such a child may be a pre-existing, live table, so the validation scan
/// blocks it under ACCESS EXCLUSIVE for the scan's duration. The CHECK match is name-agnostic
/// per-child and does not verify the predicate implies the partition bound (it errs toward not
/// escalating — never toward silence).
pub(crate) fn attach_escalation_indices(stmts: &[RawStmt]) -> BTreeSet<usize> {
    let mut created: BTreeSet<String> = BTreeSet::new();
    let mut check_not_valid: BTreeSet<String> = BTreeSet::new();
    let mut check_prepared: BTreeSet<String> = BTreeSet::new();
    let mut escalate: BTreeSet<usize> = BTreeSet::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        // Decide using state from strictly-earlier statements; an ATTACH statement never also
        // creates a table or adds/validates a constraint, so the order within the body is moot.
        if let Some(child) = attach_partition_child(node) {
            if !created.contains(&child) && !check_prepared.contains(&child) {
                escalate.insert(i);
            }
        }
        if let Some(key) = created_table_key(node) {
            created.insert(key);
        }
        if let Some((key, immediately_valid)) = added_check_constraint(node) {
            if immediately_valid {
                check_prepared.insert(key);
            } else {
                check_not_valid.insert(key);
            }
        }
        if let Some(key) = validated_constraint_table(node) {
            if check_not_valid.contains(&key) {
                check_prepared.insert(key);
            }
        }
    }
    escalate
}

/// Escalate `attach-partition` findings from `Warning` to `Error` for an `ATTACH PARTITION` of a
/// child that is neither created nor CHECK-prepared earlier in this migration (see
/// [`attach_escalation_indices`]). The escalated finding gets one sentence appended explaining why
/// the pre-existing child makes the operation error-grade. All other findings are untouched.
/// Runs before user `severity_overrides`, so an explicit override still has the final say.
pub(crate) fn escalate_pre_existing_attach(stmts: &[RawStmt], findings: &mut [Finding]) {
    let escalate = attach_escalation_indices(stmts);
    if escalate.is_empty() {
        return;
    }
    for f in findings.iter_mut() {
        if f.rule_id == "attach-partition" && escalate.contains(&f.statement_index) {
            f.severity = Severity::Error;
            f.message.push_str(
                " The child table is not created in this migration, so it may already be \
                 receiving traffic; the validation scan blocks it under ACCESS EXCLUSIVE for the \
                 scan's duration.",
            );
        }
    }
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

    fn stmts_of(sql: &str) -> Vec<RawStmt> {
        pg_query::parse(sql).unwrap().protobuf.stmts
    }

    #[test]
    fn added_check_constraint_classifies_validity() {
        assert_eq!(
            added_check_constraint(&first_node("ALTER TABLE t ADD CONSTRAINT c CHECK (a > 0)")),
            Some(("t".to_string(), true)) // plain ADD CHECK is immediately valid
        );
        assert_eq!(
            added_check_constraint(&first_node(
                "ALTER TABLE t ADD CONSTRAINT c CHECK (a > 0) NOT VALID"
            )),
            Some(("t".to_string(), false)) // NOT VALID: not enforced until VALIDATE
        );
        // A non-CHECK constraint (FK) is not a CHECK preparation.
        assert_eq!(
            added_check_constraint(&first_node(
                "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (x) REFERENCES p (id)"
            )),
            None
        );
        assert_eq!(
            added_check_constraint(&first_node("ALTER TABLE t ADD COLUMN x int")),
            None
        );
    }

    #[test]
    fn validated_constraint_table_matches_validate() {
        assert_eq!(
            validated_constraint_table(&first_node("ALTER TABLE t VALIDATE CONSTRAINT c"))
                .as_deref(),
            Some("t")
        );
        assert_eq!(
            validated_constraint_table(&first_node("ALTER TABLE t ADD COLUMN x int")),
            None
        );
    }

    #[test]
    fn escalates_attach_of_pre_existing_child_without_check() {
        // Pre-existing child, no CHECK prep -> escalate the single statement (index 0).
        let idx = attach_escalation_indices(&stmts_of(
            "ALTER TABLE parent ATTACH PARTITION child FOR VALUES FROM (0) TO (100)",
        ));
        assert!(idx.contains(&0));
    }

    #[test]
    fn does_not_escalate_same_migration_created_child() {
        // Child created in this migration -> not yet in service -> no escalation.
        let idx = attach_escalation_indices(&stmts_of(
            "CREATE TABLE child (id int); \
             ALTER TABLE parent ATTACH PARTITION child FOR VALUES FROM (0) TO (100)",
        ));
        assert!(idx.is_empty());
    }

    #[test]
    fn does_not_escalate_when_not_valid_check_then_validated() {
        // The documented safe rewrite: ADD CHECK ... NOT VALID, then VALIDATE, then ATTACH.
        let idx = attach_escalation_indices(&stmts_of(
            "ALTER TABLE child ADD CONSTRAINT cc CHECK (id >= 0 AND id < 100) NOT VALID; \
             ALTER TABLE child VALIDATE CONSTRAINT cc; \
             ALTER TABLE parent ATTACH PARTITION child FOR VALUES FROM (0) TO (100)",
        ));
        assert!(idx.is_empty());
    }

    #[test]
    fn does_not_escalate_when_plain_check_present() {
        // A plain (immediately valid) ADD CHECK before the ATTACH also prepares the child.
        let idx = attach_escalation_indices(&stmts_of(
            "ALTER TABLE child ADD CONSTRAINT cc CHECK (id >= 0 AND id < 100); \
             ALTER TABLE parent ATTACH PARTITION child FOR VALUES FROM (0) TO (100)",
        ));
        assert!(idx.is_empty());
    }

    #[test]
    fn escalates_when_not_valid_check_never_validated() {
        // ADD CHECK ... NOT VALID without a following VALIDATE does not let ATTACH skip the scan.
        let idx = attach_escalation_indices(&stmts_of(
            "ALTER TABLE child ADD CONSTRAINT cc CHECK (id >= 0 AND id < 100) NOT VALID; \
             ALTER TABLE parent ATTACH PARTITION child FOR VALUES FROM (0) TO (100)",
        ));
        assert!(idx.contains(&1));
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
