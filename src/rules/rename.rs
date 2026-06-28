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
        let kind = match node {
            NodeEnum::RenameStmt(stmt) => match ObjectType::try_from(stmt.rename_type) {
                Ok(ObjectType::ObjectTable) => "table",
                Ok(ObjectType::ObjectColumn) => "column",
                Ok(ObjectType::ObjectIndex) => "index",
                Ok(ObjectType::ObjectTabconstraint) => "constraint",
                Ok(ObjectType::ObjectView) => "view",
                Ok(ObjectType::ObjectMatview) => "materialized view",
                Ok(ObjectType::ObjectSequence) => "sequence",
                Ok(ObjectType::ObjectSchema) => "schema",
                Ok(ObjectType::ObjectType) => "type",
                Ok(ObjectType::ObjectAttribute) => "type attribute",
                Ok(ObjectType::ObjectFunction) => "function",
                Ok(ObjectType::ObjectProcedure) => "procedure",
                Ok(ObjectType::ObjectDomain) => "domain",
                _ => return,
            },
            // ALTER TYPE ... RENAME VALUE 'old' TO 'new' renames an enum label. ADD VALUE leaves
            // old_val empty and is a different operation, not a rename.
            NodeEnum::AlterEnumStmt(stmt) if !stmt.old_val.is_empty() => "enum value",
            _ => return,
        };
        out.push(RuleHit {
            message: format!(
                "Renaming this {kind} breaks every application query, view, and function that \
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
    use crate::{lint_sql, LintOptions};

    #[test]
    fn flags_table_rename() {
        let findings = lint_sql("ALTER TABLE t RENAME TO t2", &LintOptions::default()).unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_column_rename() {
        let findings = lint_sql(
            "ALTER TABLE t RENAME COLUMN a TO b",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_index_rename() {
        let findings = lint_sql("ALTER INDEX idx RENAME TO idx2", &LintOptions::default()).unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_constraint_rename() {
        let findings = lint_sql(
            "ALTER TABLE t RENAME CONSTRAINT ck TO ck2",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_view_rename() {
        let findings = lint_sql("ALTER VIEW v RENAME TO v2", &LintOptions::default()).unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_sequence_rename() {
        let findings = lint_sql("ALTER SEQUENCE s RENAME TO s2", &LintOptions::default()).unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_matview_rename() {
        let findings = lint_sql(
            "ALTER MATERIALIZED VIEW m RENAME TO m2",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_type_rename() {
        let findings =
            lint_sql("ALTER TYPE mood RENAME TO feeling", &LintOptions::default()).unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_type_attribute_rename() {
        let findings = lint_sql(
            "ALTER TYPE pt RENAME ATTRIBUTE x TO y",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_enum_value_rename() {
        let findings = lint_sql(
            "ALTER TYPE mood RENAME VALUE 'happy' TO 'glad'",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_function_rename() {
        let findings = lint_sql(
            "ALTER FUNCTION get_user(int) RENAME TO fetch_user",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_procedure_rename() {
        let findings = lint_sql(
            "ALTER PROCEDURE charge(uuid) RENAME TO process_charge",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn flags_domain_rename() {
        let findings = lint_sql(
            "ALTER DOMAIN us_postal RENAME TO zip",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().any(|f| f.rule_id == "rename"));
    }

    #[test]
    fn ignores_enum_add_value() {
        // ADD VALUE is not a rename (empty old_val) and must not fire.
        let findings = lint_sql(
            "ALTER TYPE mood ADD VALUE 'excited'",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings.iter().all(|f| f.rule_id != "rename"));
    }
}
