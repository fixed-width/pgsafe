use crate::ast::protobuf::ConstrType;
use crate::ast::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddPrimaryKeyWithoutIndex;

impl Rule for AddPrimaryKeyWithoutIndex {
    fn id(&self) -> &'static str {
        "add-primary-key-without-index"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        let via_constraint = super::constraints_being_added(node).into_iter().any(|c| {
            matches!(
                ConstrType::try_from(c.contype),
                Ok(ConstrType::ConstrPrimary)
            ) && c.indexname.is_empty()
        });

        let via_column = super::columns_being_added(node)
            .into_iter()
            .any(|col| super::column_has_constraint(col, ConstrType::ConstrPrimary));

        if via_constraint {
            out.push(RuleHit {
                message: "Adding a PRIMARY KEY inline builds its unique index (and may scan for NOT NULL) \
                          under an ACCESS EXCLUSIVE lock."
                    .into(),
                guidance: "Build the index with CREATE UNIQUE INDEX CONCURRENTLY, then attach it: \
                           ALTER TABLE ... ADD CONSTRAINT ... PRIMARY KEY USING INDEX idx."
                    .into(),
                fix: None,
            });
        }

        if via_column {
            out.push(RuleHit {
                message: "Adding a column with an inline PRIMARY KEY builds a unique index \
                          (and enforces NOT NULL) under an ACCESS EXCLUSIVE lock."
                    .into(),
                guidance: "Add the column nullable first, backfill existing rows, build a unique index with \
                           CREATE UNIQUE INDEX CONCURRENTLY, then attach it \
                           (ALTER TABLE ... ADD CONSTRAINT ... PRIMARY KEY USING INDEX idx) \
                           and SET NOT NULL."
                    .into(),
                fix: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn column_path_guidance_mentions_nullable_not_table_path() {
        let findings = crate::lint_sql(
            "ALTER TABLE t ADD COLUMN id int PRIMARY KEY",
            &crate::LintOptions::default(),
        )
        .unwrap();
        let f = findings
            .iter()
            .find(|f| f.rule_id == "add-primary-key-without-index")
            .expect("rule must fire for inline column PK");
        assert!(
            f.guidance.contains("nullable"),
            "column-path guidance must instruct adding the column nullable first"
        );
    }
}
