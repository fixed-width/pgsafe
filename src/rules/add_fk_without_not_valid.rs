use pg_query::protobuf::{AlterTableType, ConstrType};
use pg_query::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct AddFkWithoutNotValid;

impl Rule for AddFkWithoutNotValid {
    fn id(&self) -> &'static str {
        "add-fk-without-not-valid"
    }

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
            if c.contype == ConstrType::ConstrForeign as i32 && !c.skip_validation {
                out.push(RuleHit {
                    message: "Adding a FOREIGN KEY without NOT VALID validates every existing row \
                              while holding locks on both tables."
                        .into(),
                    guidance: "Add the constraint with NOT VALID first (brief lock, no scan), then run \
                               ALTER TABLE ... VALIDATE CONSTRAINT in a separate statement (it allows \
                               concurrent reads and writes)."
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
    fn flags_fk_without_not_valid() {
        let sql = "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id)";
        let findings = lint_sql(sql).unwrap();
        assert!(findings
            .iter()
            .any(|f| f.rule_id == "add-fk-without-not-valid"));
    }

    #[test]
    fn ignores_fk_with_not_valid() {
        let sql = "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id) NOT VALID";
        let findings = lint_sql(sql).unwrap();
        assert!(findings
            .iter()
            .all(|f| f.rule_id != "add-fk-without-not-valid"));
    }
}
