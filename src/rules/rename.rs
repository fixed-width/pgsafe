use pg_query::protobuf::ObjectType;
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct Rename;

impl Rule for Rename {
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        let NodeEnum::RenameStmt(stmt) = node else {
            return;
        };
        let kind = if stmt.rename_type == ObjectType::ObjectTable as i32 {
            "table"
        } else if stmt.rename_type == ObjectType::ObjectColumn as i32 {
            "column"
        } else {
            return;
        };
        out.push(RuleHit {
            rule_id: "rename",
            severity: Severity::Warning,
            message: format!(
                "Renaming a {kind} breaks every application query, view, and function that \
                 references the old name."
            ),
            guidance: "Avoid renames in a rolling deploy. Prefer expand/contract: add the new name, \
                       dual-write, migrate readers, then drop the old name — or use a view to alias \
                       during the transition."
                .into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::lint_sql;

    #[test]
    fn flags_table_rename() {
        let findings = lint_sql("ALTER TABLE t RENAME TO t2").unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_column_rename() {
        let findings = lint_sql("ALTER TABLE t RENAME COLUMN a TO b").unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }
}
