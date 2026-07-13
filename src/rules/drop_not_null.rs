use crate::ast::protobuf::AlterTableType;
use crate::ast::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct DropNotNull;

impl Rule for DropNotNull {
    fn id(&self) -> &'static str {
        "drop-not-null"
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for cmd in super::alter_table_cmds(node) {
            if matches!(
                AlterTableType::try_from(cmd.subtype),
                Ok(AlterTableType::AtDropNotNull)
            ) {
                out.push(RuleHit {
                    message: "ALTER COLUMN ... DROP NOT NULL silently removes a not-null invariant that \
                              application code, ORMs, and the query planner may rely on."
                        .into(),
                    guidance: "Confirm nothing depends on the column being non-null — planner assumptions, \
                               ORM models, application code — before dropping the constraint."
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
    fn flags_drop_not_null() {
        let f = findings("ALTER TABLE t ALTER COLUMN a DROP NOT NULL")
            .into_iter()
            .find(|f| f.rule_id == "drop-not-null")
            .expect("rule must fire");
        assert_eq!(f.severity, Severity::Warning);
    }

    #[test]
    fn silent_on_set_not_null() {
        assert!(findings("ALTER TABLE t ALTER COLUMN a SET NOT NULL")
            .iter()
            .all(|f| f.rule_id != "drop-not-null"));
    }
}
