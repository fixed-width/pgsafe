use pg_query::protobuf::AlterTableType;
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct SetNotNull;

impl Rule for SetNotNull {
    fn id(&self) -> &'static str {
        "set-not-null"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for cmd in super::alter_table_cmds(node) {
            if matches!(
                AlterTableType::try_from(cmd.subtype),
                Ok(AlterTableType::AtSetNotNull)
            ) {
                out.push(RuleHit {
                    message: "ALTER COLUMN ... SET NOT NULL scans the entire table under an ACCESS \
                              EXCLUSIVE lock."
                        .into(),
                    guidance: "On PG12+, first add `CHECK (col IS NOT NULL) NOT VALID`, run VALIDATE \
                               CONSTRAINT, then SET NOT NULL (it reuses the validated check and skips \
                               the scan). Drop the helper CHECK afterward if you like."
                        .into(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions};

    #[test]
    fn flags_set_not_null() {
        let findings = lint_sql(
            "ALTER TABLE t ALTER COLUMN a SET NOT NULL",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "set-not-null"));
    }

    #[test]
    fn ignores_drop_not_null() {
        let findings = lint_sql(
            "ALTER TABLE t ALTER COLUMN a DROP NOT NULL",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().all(|f| f.rule_id != "set-not-null"));
    }
}
