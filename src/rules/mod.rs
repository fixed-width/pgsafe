//! Rule engine: the `Rule` trait and the registry of enabled rules.

use std::sync::LazyLock;

use pg_query::protobuf::{
    AlterTableCmd, AlterTableType, ColumnDef, ConstrType, Constraint, DefElem,
};
use pg_query::NodeEnum;

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
mod add_column_not_null_no_default;
mod add_fk_without_not_valid;
mod add_index_non_concurrent;
mod add_primary_key_without_index;
mod add_unique_constraint;
mod alter_column_type;
mod drop_column;
mod drop_index_non_concurrent;
mod drop_table;
mod reindex_non_concurrent;
mod rename;
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
        Box::new(drop_column::DropColumn),
        Box::new(truncate::Truncate),
        Box::new(vacuum_full_cluster::VacuumFullOrCluster),
        Box::new(reindex_non_concurrent::ReindexNonConcurrent),
        Box::new(add_unique_constraint::AddUniqueConstraint),
        Box::new(add_primary_key_without_index::AddPrimaryKeyWithoutIndex),
        Box::new(add_column_not_null_no_default::AddColumnNotNullNoDefault),
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
fn alter_table_cmds(node: &NodeEnum) -> Vec<&AlterTableCmd> {
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
fn column_has_constraint(col: &ColumnDef, contype: ConstrType) -> bool {
    col.constraints.iter().any(|cn| {
        matches!(cn.node.as_ref(), Some(NodeEnum::Constraint(con))
            if ConstrType::try_from(con.contype) == Ok(contype))
    })
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

/// Mirrors PostgreSQL's `defGetBoolean`: a `DefElem` with no `arg` (flag present but no
/// explicit value) defaults to `true`. For explicit args, integer 0 is false, non-zero is
/// true; strings "false"/"off"/"0"/"f"/"no"/"n" (case-insensitive) are false; all other
/// strings and boolean-true literals are true.
pub(super) fn defelem_is_true(de: &DefElem) -> bool {
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
                "drop-column",
                "truncate",
                "vacuum-full-cluster",
                "reindex-non-concurrent",
                "add-unique-constraint",
                "add-primary-key-without-index",
                "add-column-not-null-no-default",
            ]
        );
    }
}
