use pg_query::protobuf::AlterTableType;
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AlterColumnType;

impl Rule for AlterColumnType {
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        let NodeEnum::AlterTableStmt(stmt) = node else { return };
        for cmd_node in &stmt.cmds {
            let Some(NodeEnum::AlterTableCmd(cmd)) = cmd_node.node.as_ref() else { continue };
            if cmd.subtype == AlterTableType::AtAlterColumnType as i32 {
                out.push(RuleHit {
                    rule_id: "alter-column-type",
                    severity: Severity::Warning,
                    message: "ALTER COLUMN ... TYPE usually rewrites the whole table and rebuilds its \
                              indexes under an ACCESS EXCLUSIVE lock."
                        .into(),
                    guidance: "Prefer a no-rewrite type change where possible (e.g. increasing a \
                               varchar length, or varchar->text). Otherwise use expand/contract: add a \
                               new column, dual-write and backfill in batches, then swap. Note some \
                               changes (e.g. int->bigint) always rewrite."
                        .into(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lint_sql;

    #[test]
    fn flags_alter_column_type() {
        let findings = lint_sql("ALTER TABLE t ALTER COLUMN a TYPE bigint").unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "alter-column-type"));
    }

    #[test]
    fn ignores_unrelated_alter() {
        let findings = lint_sql("ALTER TABLE t ALTER COLUMN a SET DEFAULT 0").unwrap();
        assert!(findings.iter().all(|f| f.rule_id != "alter-column-type"));
    }
}
