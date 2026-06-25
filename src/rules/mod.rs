//! Rule engine: the `Rule` trait and the registry of enabled rules.

use std::sync::LazyLock;

use pg_query::protobuf::{AlterTableCmd, AlterTableType, Constraint};
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
mod add_fk_without_not_valid;
mod alter_column_type;
mod drop_column;
mod drop_index_non_concurrent;
mod drop_table;
mod non_concurrent_index;
mod rename;
mod set_not_null;

static RULES: LazyLock<Vec<Box<dyn Rule>>> = LazyLock::new(|| {
    vec![
        Box::new(non_concurrent_index::NonConcurrentIndex),
        Box::new(add_fk_without_not_valid::AddFkWithoutNotValid),
        Box::new(add_check_without_not_valid::AddCheckWithoutNotValid),
        Box::new(set_not_null::SetNotNull),
        Box::new(alter_column_type::AlterColumnType),
        Box::new(rename::Rename),
        Box::new(drop_index_non_concurrent::DropIndexNonConcurrent),
        Box::new(drop_table::DropTable),
        Box::new(drop_column::DropColumn),
    ]
});

/// All rules enabled in this build, in stable registration order.
pub(crate) fn all_rules() -> &'static [Box<dyn Rule>] {
    &RULES
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

#[cfg(test)]
mod tests {
    use super::all_rules;

    #[test]
    fn registration_order_is_stable() {
        let ids: Vec<&str> = all_rules().iter().map(|r| r.id()).collect();
        assert_eq!(
            ids,
            [
                "non-concurrent-index",
                "add-fk-without-not-valid",
                "add-check-without-not-valid",
                "set-not-null",
                "alter-column-type",
                "rename",
                "drop-index-non-concurrent",
                "drop-table",
                "drop-column",
            ]
        );
    }
}
