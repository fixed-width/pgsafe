use pg_query::protobuf::ObjectType;
use pg_query::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct Rename;

impl Rule for Rename {
    fn id(&self) -> &'static str {
        "rename"
    }

    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        let NodeEnum::RenameStmt(stmt) = node else {
            return;
        };
        let kind = match ObjectType::try_from(stmt.rename_type) {
            Ok(ObjectType::ObjectTable) => "table",
            Ok(ObjectType::ObjectColumn) => "column",
            Ok(ObjectType::ObjectIndex) => "index",
            Ok(ObjectType::ObjectTabconstraint) => "constraint",
            Ok(ObjectType::ObjectView) => "view",
            Ok(ObjectType::ObjectMatview) => "materialized view",
            Ok(ObjectType::ObjectSequence) => "sequence",
            Ok(ObjectType::ObjectSchema) => "schema",
            _ => return,
        };
        out.push(RuleHit {
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

    #[test]
    fn flags_index_rename() {
        let findings = lint_sql("ALTER INDEX idx RENAME TO idx2").unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_constraint_rename() {
        let findings = lint_sql("ALTER TABLE t RENAME CONSTRAINT ck TO ck2").unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_view_rename() {
        let findings = lint_sql("ALTER VIEW v RENAME TO v2").unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_sequence_rename() {
        let findings = lint_sql("ALTER SEQUENCE s RENAME TO s2").unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_matview_rename() {
        let findings = lint_sql("ALTER MATERIALIZED VIEW m RENAME TO m2").unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }
}
