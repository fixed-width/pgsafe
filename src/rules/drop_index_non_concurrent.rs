use pg_query::protobuf::ObjectType;
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct DropIndexNonConcurrent;

impl Rule for DropIndexNonConcurrent {
    fn id(&self) -> &'static str {
        "drop-index-non-concurrent"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        if let NodeEnum::DropStmt(d) = node {
            if matches!(
                ObjectType::try_from(d.remove_type),
                Ok(ObjectType::ObjectIndex)
            ) && !d.concurrent
            {
                out.push(RuleHit {
                    message: "DROP INDEX without CONCURRENTLY takes an ACCESS EXCLUSIVE lock on the index's \
                              table, blocking reads and writes while it runs."
                        .into(),
                    guidance: "Use DROP INDEX CONCURRENTLY (outside a transaction block)."
                        .into(),
                    // CONCURRENTLY is illegal for multi-index drops; gate on exactly one object.
                    fix: (d.objects.len() == 1).then(|| crate::fix::FixDraft {
                        title: "Add CONCURRENTLY",
                        edits: vec![crate::fix::FixDraftEdit {
                            anchor: crate::fix::FixAnchor::AfterKeyword("INDEX"),
                            replacement: " CONCURRENTLY".into(),
                        }],
                    }),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions};

    fn findings(sql: &str) -> Vec<crate::Finding> {
        lint_sql(sql, &LintOptions::default()).unwrap()
    }

    #[test]
    fn flags_plain_drop_index() {
        assert!(findings("DROP INDEX idx")
            .iter()
            .any(|f| f.rule_id == "drop-index-non-concurrent"));
    }

    #[test]
    fn ignores_concurrent_drop_index() {
        assert!(findings("DROP INDEX CONCURRENTLY idx")
            .iter()
            .all(|f| f.rule_id != "drop-index-non-concurrent"));
    }

    #[test]
    fn emits_a_concurrently_fix() {
        use crate::fix::apply;
        let sql = "DROP INDEX idx;";
        let fs = findings(sql);
        let f = fs
            .iter()
            .find(|f| f.rule_id == "drop-index-non-concurrent")
            .unwrap();
        let fix = f.fix.as_ref().expect("fix present");
        assert_eq!(fix.title, "Add CONCURRENTLY");
        let fixed = apply(sql, fix);
        assert_eq!(fixed, "DROP INDEX CONCURRENTLY idx;");
        // Applying it clears the finding.
        assert!(findings(&fixed)
            .iter()
            .all(|f| f.rule_id != "drop-index-non-concurrent"));
    }

    #[test]
    fn multi_index_drop_has_no_fix() {
        let fs = findings("DROP INDEX a, b;");
        let f = fs
            .iter()
            .find(|f| f.rule_id == "drop-index-non-concurrent")
            .expect("rule must fire for multi-index drop");
        assert!(
            f.fix.is_none(),
            "multi-index DROP INDEX must not produce a fix"
        );
    }
}
