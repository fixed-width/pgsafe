//! Rule engine: the `Rule` trait and the registry of enabled rules.

use std::sync::LazyLock;

use crate::ast::protobuf::{
    AlterTableCmd, AlterTableType, ColumnDef, ConstrType, Constraint, DefElem, ReindexStmt,
};
use crate::ast::NodeEnum;

use crate::{RuleHit, Severity};

/// A single safety rule.
pub(crate) trait Rule: Send + Sync {
    /// Stable kebab-case id — the public contract key, unique across the registry.
    fn id(&self) -> &'static str;
    /// Severity for every hit this rule emits.
    fn severity(&self) -> Severity {
        Severity::Warning
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>);
}

mod add_check_without_not_valid;
mod add_column_generated_stored;
mod add_column_identity;
mod add_column_not_null_no_default;
mod add_column_serial;
mod add_column_volatile_default;
mod add_exclusion_constraint;
mod add_fk_without_not_valid;
mod add_index_non_concurrent;
mod add_primary_key_without_index;
mod add_trigger;
mod add_unique_constraint;
mod alter_column_type;
mod attach_partition;
mod detach_partition_non_concurrent;
mod drop_column;
mod drop_constraint;
mod drop_database;
mod drop_index_non_concurrent;
mod drop_table;
mod prefer_bigint_primary_key;
mod prefer_jsonb;
mod refresh_matview_non_concurrent;
mod reindex_non_concurrent;
mod rename;
mod set_access_method;
mod set_logged_unlogged;
mod set_not_null;
mod truncate;
mod vacuum_full_cluster;

static RULES: LazyLock<Vec<Box<dyn Rule>>> = LazyLock::new(|| {
    vec![
        Box::new(add_index_non_concurrent::AddIndexNonConcurrent),
        Box::new(add_fk_without_not_valid::AddFkWithoutNotValid),
        Box::new(add_check_without_not_valid::AddCheckWithoutNotValid),
        Box::new(set_not_null::SetNotNull),
        Box::new(alter_column_type::AlterColumnType),
        Box::new(rename::Rename),
        Box::new(drop_index_non_concurrent::DropIndexNonConcurrent),
        Box::new(drop_table::DropTable),
        Box::new(drop_database::DropDatabase),
        Box::new(drop_column::DropColumn),
        Box::new(truncate::Truncate),
        Box::new(vacuum_full_cluster::VacuumFullOrCluster),
        Box::new(reindex_non_concurrent::ReindexNonConcurrent),
        Box::new(add_unique_constraint::AddUniqueConstraint),
        Box::new(add_primary_key_without_index::AddPrimaryKeyWithoutIndex),
        Box::new(add_column_not_null_no_default::AddColumnNotNullNoDefault),
        Box::new(add_column_volatile_default::AddColumnVolatileDefault),
        Box::new(add_column_serial::AddColumnSerial),
        Box::new(add_column_identity::AddColumnIdentity),
        Box::new(add_column_generated_stored::AddColumnGeneratedStored),
        Box::new(set_logged_unlogged::SetLoggedUnlogged),
        Box::new(refresh_matview_non_concurrent::RefreshMatviewNonConcurrent),
        Box::new(add_exclusion_constraint::AddExclusionConstraint),
        Box::new(prefer_jsonb::PreferJsonb),
        Box::new(prefer_bigint_primary_key::PreferBigintPrimaryKey),
        Box::new(drop_constraint::DropConstraint),
        Box::new(add_trigger::AddTrigger),
        Box::new(detach_partition_non_concurrent::DetachPartitionNonConcurrent),
        Box::new(attach_partition::AttachPartition),
        Box::new(set_access_method::SetAccessMethod),
    ]
});

/// All rules enabled in this build, in stable registration order.
pub(crate) fn all_rules() -> &'static [Box<dyn Rule>] {
    &RULES
}

/// The ids of every enabled AST rule, in registration order.
pub(crate) fn rule_ids() -> Vec<&'static str> {
    all_rules().iter().map(|r| r.id()).collect()
}

/// All `AlterTableCmd`s in an `ALTER TABLE` statement (empty for any other node).
pub(crate) fn alter_table_cmds(node: &NodeEnum) -> Vec<&AlterTableCmd> {
    let NodeEnum::AlterTableStmt(stmt) = node else {
        return Vec::new();
    };
    stmt.cmds
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            NodeEnum::AlterTableCmd(cmd) => Some(cmd.as_ref()),
            _ => None,
        })
        .collect()
}

/// All `ColumnDef`s being added by `ADD COLUMN` commands in an `ALTER TABLE`.
fn columns_being_added(node: &NodeEnum) -> Vec<&ColumnDef> {
    alter_table_cmds(node)
        .into_iter()
        .filter(|cmd| {
            matches!(
                AlterTableType::try_from(cmd.subtype),
                Ok(AlterTableType::AtAddColumn)
            )
        })
        .filter_map(|cmd| match cmd.def.as_ref()?.node.as_ref()? {
            NodeEnum::ColumnDef(c) => Some(c.as_ref()),
            _ => None,
        })
        .collect()
}

/// Whether a column definition carries an inline constraint of the given type.
pub(crate) fn column_has_constraint(col: &ColumnDef, contype: ConstrType) -> bool {
    col.constraints.iter().any(|cn| {
        matches!(cn.node.as_ref(), Some(NodeEnum::Constraint(con))
            if ConstrType::try_from(con.contype) == Ok(contype))
    })
}

/// Column definitions whose declared type is statically visible: those in a `CREATE TABLE`
/// and those added by `ALTER TABLE ... ADD COLUMN`. Used by the schema-design lints.
pub(crate) fn defined_columns(node: &NodeEnum) -> Vec<&ColumnDef> {
    match node {
        NodeEnum::CreateStmt(c) => c
            .table_elts
            .iter()
            .filter_map(|n| match n.node.as_ref()? {
                NodeEnum::ColumnDef(col) => Some(col.as_ref()),
                _ => None,
            })
            .collect(),
        NodeEnum::AlterTableStmt(_) => columns_being_added(node),
        _ => Vec::new(),
    }
}

/// The base type name of a column — the last element of its `TypeName.names`, lowercased
/// (e.g. `json`, `jsonb`, `int4`, `serial`, `varchar`), or `None` if absent.
pub(crate) fn column_base_type(col: &ColumnDef) -> Option<String> {
    match col.type_name.as_ref()?.names.last()?.node.as_ref()? {
        NodeEnum::String(s) => Some(s.sval.to_ascii_lowercase()),
        _ => None,
    }
}

/// All constraints added by `ADD CONSTRAINT` commands in an `ALTER TABLE`.
fn constraints_being_added(node: &NodeEnum) -> Vec<&Constraint> {
    alter_table_cmds(node)
        .into_iter()
        .filter(|cmd| {
            matches!(
                AlterTableType::try_from(cmd.subtype),
                Ok(AlterTableType::AtAddConstraint)
            )
        })
        .filter_map(|cmd| match cmd.def.as_ref()?.node.as_ref()? {
            NodeEnum::Constraint(c) => Some(c.as_ref()),
            _ => None,
        })
        .collect()
}

/// Table-level constraints a statement introduces: the `Constraint` elements of a
/// `CREATE TABLE`, or those added by `ALTER TABLE ... ADD CONSTRAINT`. Column-level
/// inline constraints are not included (read them from each `ColumnDef.constraints`).
pub(crate) fn defined_table_constraints(node: &NodeEnum) -> Vec<&Constraint> {
    match node {
        NodeEnum::CreateStmt(c) => c
            .table_elts
            .iter()
            .filter_map(|n| match n.node.as_ref()? {
                NodeEnum::Constraint(con) => Some(con.as_ref()),
                _ => None,
            })
            .collect(),
        NodeEnum::AlterTableStmt(_) => constraints_being_added(node),
        _ => Vec::new(),
    }
}

/// A `REINDEX ... CONCURRENTLY` (a `concurrently` option that is true).
pub(crate) fn reindex_is_concurrent(r: &ReindexStmt) -> bool {
    r.params.iter().any(|p| {
        matches!(p.node.as_ref(), Some(NodeEnum::DefElem(de))
            if de.defname == "concurrently" && defelem_is_true(de))
    })
}

/// Mirrors PostgreSQL's `defGetBoolean`: a `DefElem` with no `arg` (flag present but no
/// explicit value) defaults to `true`. For explicit args, integer 0 is false, non-zero is
/// true; strings "false"/"off"/"0"/"f"/"no"/"n" (case-insensitive) are false; all other
/// strings and boolean-true literals are true.
pub(crate) fn defelem_is_true(de: &DefElem) -> bool {
    match de.arg.as_deref().and_then(|n| n.node.as_ref()) {
        None => true,
        Some(NodeEnum::Boolean(b)) => b.boolval,
        Some(NodeEnum::Integer(i)) => i.ival != 0,
        Some(NodeEnum::String(s)) => !matches!(
            s.sval.to_ascii_lowercase().as_str(),
            "false" | "off" | "0" | "f" | "no" | "n"
        ),
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::all_rules;

    #[test]
    fn severity_classification_is_locked() {
        use crate::Severity;
        use std::collections::HashMap;
        let sev: HashMap<&str, Severity> =
            all_rules().iter().map(|r| (r.id(), r.severity())).collect();
        let errors = [
            "add-index-non-concurrent",
            "reindex-non-concurrent",
            "drop-index-non-concurrent",
            "alter-column-type",
            "set-not-null",
            "add-fk-without-not-valid",
            "add-check-without-not-valid",
            "vacuum-full-cluster",
            "add-unique-constraint",
            "add-primary-key-without-index",
            "add-column-not-null-no-default",
            "add-column-volatile-default",
            "add-column-serial",
            "add-column-identity",
            "add-column-generated-stored",
            "set-logged-unlogged",
            "refresh-matview-non-concurrent",
            "add-exclusion-constraint",
            "detach-partition-non-concurrent",
            "set-access-method",
        ];
        let warnings = [
            "rename",
            "drop-table",
            "drop-database",
            "drop-column",
            "truncate",
            "prefer-jsonb",
            "prefer-bigint-primary-key",
            "drop-constraint",
            "add-trigger",
            // Nominal Warning; escalated to Error at runtime for a pre-existing child (no
            // in-migration CHECK prep) — see newtable::escalate_pre_existing_attach.
            "attach-partition",
        ];
        for id in errors {
            assert_eq!(sev[id], Severity::Error, "{id} should be error");
        }
        for id in warnings {
            assert_eq!(sev[id], Severity::Warning, "{id} should be warning");
        }
        assert_eq!(
            errors.len() + warnings.len(),
            sev.len(),
            "every rule must be classified"
        );
    }

    #[test]
    fn registration_order_is_stable() {
        let ids: Vec<&str> = all_rules().iter().map(|r| r.id()).collect();
        assert_eq!(
            ids,
            [
                "add-index-non-concurrent",
                "add-fk-without-not-valid",
                "add-check-without-not-valid",
                "set-not-null",
                "alter-column-type",
                "rename",
                "drop-index-non-concurrent",
                "drop-table",
                "drop-database",
                "drop-column",
                "truncate",
                "vacuum-full-cluster",
                "reindex-non-concurrent",
                "add-unique-constraint",
                "add-primary-key-without-index",
                "add-column-not-null-no-default",
                "add-column-volatile-default",
                "add-column-serial",
                "add-column-identity",
                "add-column-generated-stored",
                "set-logged-unlogged",
                "refresh-matview-non-concurrent",
                "add-exclusion-constraint",
                "prefer-jsonb",
                "prefer-bigint-primary-key",
                "drop-constraint",
                "add-trigger",
                "detach-partition-non-concurrent",
                "attach-partition",
                "set-access-method",
            ]
        );
    }
}
