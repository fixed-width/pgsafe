use crate::ast::protobuf::AlterTableType;
use crate::ast::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AlterColumnType;

impl Rule for AlterColumnType {
    fn id(&self) -> &'static str {
        "alter-column-type"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for cmd in super::alter_table_cmds(node) {
            if matches!(
                AlterTableType::try_from(cmd.subtype),
                Ok(AlterTableType::AtAlterColumnType)
            ) {
                out.push(RuleHit {
                    message: "ALTER COLUMN ... TYPE usually rewrites the whole table and rebuilds its \
                              indexes under an ACCESS EXCLUSIVE lock. Even a metadata-only change that \
                              does not rewrite (widening a varchar/numeric/timestamp precision, or \
                              varchar->text) changes the column's result type and breaks cached query \
                              plans and prepared statements in live sessions ('cached plan must not \
                              change result type') until they re-plan."
                        .into(),
                    guidance: "Use expand/contract for a rewriting change: add a new column, dual-write \
                               and backfill in batches, then swap (some changes, e.g. int->bigint, \
                               always rewrite). A no-rewrite change (e.g. varchar->text or widening a \
                               varchar) avoids the table rewrite but still invalidates cached plans, so \
                               recycle pooled connections or run DISCARD PLANS afterward, or apply it \
                               during a deploy window."
                        .into(),
                    fix: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions};

    #[test]
    fn flags_alter_column_type() {
        let findings = lint_sql(
            "ALTER TABLE t ALTER COLUMN a TYPE bigint",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "alter-column-type"));
    }

    #[test]
    fn ignores_unrelated_alter() {
        let findings = lint_sql(
            "ALTER TABLE t ALTER COLUMN a SET DEFAULT 0",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().all(|f| f.rule_id != "alter-column-type"));
    }

    #[test]
    fn message_names_the_cached_plan_hazard() {
        let findings = lint_sql(
            "ALTER TABLE t ALTER COLUMN a TYPE text",
            &LintOptions::default(),
        )
        .unwrap();
        let f = findings
            .iter()
            .find(|f| f.rule_id == "alter-column-type")
            .expect("rule must fire");
        // The message must name the cached-plan hazard of a no-rewrite change...
        assert!(
            f.message.contains("cached plan"),
            "message must mention the cached-plan hazard, got: {}",
            f.message
        );
        // ...and the guidance must not present a no-rewrite change as unconditionally safe.
        assert!(
            f.guidance.contains("DISCARD PLANS"),
            "guidance must tell users to handle cached plans, got: {}",
            f.guidance
        );
    }
}
