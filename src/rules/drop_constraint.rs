use crate::ast::protobuf::AlterTableType;
use crate::ast::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct DropConstraint;

impl Rule for DropConstraint {
    fn id(&self) -> &'static str {
        "drop-constraint"
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for cmd in super::alter_table_cmds(node) {
            if matches!(
                AlterTableType::try_from(cmd.subtype),
                Ok(AlterTableType::AtDropConstraint)
            ) {
                out.push(RuleHit {
                    message: "DROP CONSTRAINT removes an integrity guarantee (foreign key, check, or \
                              unique) that application code may rely on; dropping a primary key or \
                              unique constraint can also break logical-replication replica identity."
                        .into(),
                    guidance: "Confirm no application logic or replication setup depends on the \
                               constraint before dropping it."
                        .into(),
                    fix: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions, Severity};

    fn findings(sql: &str) -> Vec<crate::Finding> {
        lint_sql(sql, &LintOptions::default()).unwrap()
    }

    #[test]
    fn flags_drop_constraint() {
        assert!(findings("ALTER TABLE t DROP CONSTRAINT c")
            .iter()
            .any(|f| f.rule_id == "drop-constraint"));
    }

    #[test]
    fn drop_constraint_is_a_warning() {
        let f = findings("ALTER TABLE t DROP CONSTRAINT c")
            .into_iter()
            .find(|f| f.rule_id == "drop-constraint")
            .expect("rule must fire");
        assert_eq!(f.severity, Severity::Warning);
    }

    #[test]
    fn silent_on_other_alter_commands() {
        assert!(findings("ALTER TABLE t ADD COLUMN x int")
            .iter()
            .all(|f| f.rule_id != "drop-constraint"));
    }
}
