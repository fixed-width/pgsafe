use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct ReindexNonConcurrent;

impl Rule for ReindexNonConcurrent {
    fn id(&self) -> &'static str {
        "reindex-non-concurrent"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        if let NodeEnum::ReindexStmt(r) = node {
            let concurrent = r.params.iter().any(|p| {
                matches!(p.node.as_ref(), Some(NodeEnum::DefElem(de))
                    if de.defname == "concurrently" && super::defelem_is_true(de))
            });
            if !concurrent {
                // Map the reindex target type to the keyword CONCURRENTLY must follow.
                // REINDEX SYSTEM and Undefined do not support CONCURRENTLY.
                let kw = match pg_query::protobuf::ReindexObjectType::try_from(r.kind) {
                    Ok(pg_query::protobuf::ReindexObjectType::ReindexObjectIndex) => Some("INDEX"),
                    Ok(pg_query::protobuf::ReindexObjectType::ReindexObjectTable) => Some("TABLE"),
                    Ok(pg_query::protobuf::ReindexObjectType::ReindexObjectSchema) => {
                        Some("SCHEMA")
                    }
                    Ok(pg_query::protobuf::ReindexObjectType::ReindexObjectDatabase) => {
                        Some("DATABASE")
                    }
                    _ => None, // System / Undefined: no legal CONCURRENTLY form
                };
                out.push(RuleHit {
                    message: "REINDEX without CONCURRENTLY takes an ACCESS EXCLUSIVE lock on each index it \
                              rebuilds, blocking writes (and reads through that index)."
                        .into(),
                    guidance: "Use REINDEX INDEX CONCURRENTLY (PG12+, outside a transaction); on older \
                               servers use pg_repack or a maintenance window."
                        .into(),
                    fix: kw.map(|kw| crate::fix::FixDraft {
                        title: "Add CONCURRENTLY",
                        edits: vec![crate::fix::FixDraftEdit {
                            anchor: crate::fix::FixAnchor::AfterKeyword(kw),
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
    fn flags_plain_reindex_index() {
        assert!(findings("REINDEX INDEX idx")
            .iter()
            .any(|f| f.rule_id == "reindex-non-concurrent"));
    }

    #[test]
    fn ignores_concurrent_reindex_index() {
        assert!(findings("REINDEX INDEX CONCURRENTLY idx")
            .iter()
            .all(|f| f.rule_id != "reindex-non-concurrent"));
    }

    #[test]
    fn emits_a_concurrently_fix() {
        use crate::fix::apply;
        let sql = "REINDEX INDEX idx;";
        let fs = findings(sql);
        let f = fs
            .iter()
            .find(|f| f.rule_id == "reindex-non-concurrent")
            .unwrap();
        let fix = f.fix.as_ref().expect("fix present");
        assert_eq!(fix.title, "Add CONCURRENTLY");
        let fixed = apply(sql, fix);
        assert_eq!(fixed, "REINDEX INDEX CONCURRENTLY idx;");
        // Applying it clears the finding.
        assert!(findings(&fixed)
            .iter()
            .all(|f| f.rule_id != "reindex-non-concurrent"));
    }

    #[test]
    fn reindex_system_has_no_fix() {
        let fs = findings("REINDEX SYSTEM mydb;");
        let f = fs
            .iter()
            .find(|f| f.rule_id == "reindex-non-concurrent")
            .expect("rule must fire for REINDEX SYSTEM");
        assert!(f.fix.is_none(), "REINDEX SYSTEM must not produce a fix");
    }
}
