use pg_query::protobuf::{AlterTableType, ConstrType};
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddCheckWithoutNotValid;

impl Rule for AddCheckWithoutNotValid {
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        let NodeEnum::AlterTableStmt(stmt) = node else {
            return;
        };
        for cmd_node in &stmt.cmds {
            let Some(NodeEnum::AlterTableCmd(cmd)) = cmd_node.node.as_ref() else {
                continue;
            };
            if cmd.subtype != AlterTableType::AtAddConstraint as i32 {
                continue;
            }
            let Some(def) = cmd.def.as_ref() else {
                continue;
            };
            let Some(NodeEnum::Constraint(c)) = def.node.as_ref() else {
                continue;
            };
            if c.contype == ConstrType::ConstrCheck as i32 && !c.skip_validation {
                out.push(RuleHit {
                    rule_id: "add-check-without-not-valid",
                    severity: Severity::Warning,
                    message: "Adding a CHECK constraint without NOT VALID scans the whole table \
                              under an ACCESS EXCLUSIVE lock."
                        .into(),
                    guidance:
                        "Add the CHECK with NOT VALID, then run VALIDATE CONSTRAINT separately \
                               (SHARE UPDATE EXCLUSIVE — concurrent reads and writes are allowed)."
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
    fn flags_check_without_not_valid() {
        let findings = lint_sql("ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0)").unwrap();
        assert!(findings
            .iter()
            .any(|f| f.rule_id == "add-check-without-not-valid"));
    }

    #[test]
    fn ignores_check_with_not_valid() {
        let findings = lint_sql("ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0) NOT VALID").unwrap();
        assert!(findings
            .iter()
            .all(|f| f.rule_id != "add-check-without-not-valid"));
    }
}
