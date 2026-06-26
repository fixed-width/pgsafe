use pg_query::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct AddIndexNonConcurrent;

impl Rule for AddIndexNonConcurrent {
    fn id(&self) -> &'static str {
        "add-index-non-concurrent"
    }

    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        if let NodeEnum::IndexStmt(stmt) = node {
            if !stmt.concurrent {
                out.push(RuleHit {
                    message: "CREATE INDEX without CONCURRENTLY takes a lock that blocks writes \
                              to the table for the entire build."
                        .into(),
                    guidance:
                        "Use CREATE INDEX CONCURRENTLY (outside a transaction block). A failed \
                               CONCURRENTLY build leaves an INVALID index: drop it with DROP INDEX \
                               CONCURRENTLY and retry, or rebuild with REINDEX INDEX CONCURRENTLY."
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
    fn flags_plain_create_index() {
        let findings = lint_sql("CREATE INDEX idx ON t (col)").unwrap();
        assert!(findings
            .iter()
            .any(|f| f.rule_id == "add-index-non-concurrent"));
    }

    #[test]
    fn ignores_concurrent_create_index() {
        let findings = lint_sql("CREATE INDEX CONCURRENTLY idx ON t (col)").unwrap();
        assert!(findings
            .iter()
            .all(|f| f.rule_id != "add-index-non-concurrent"));
    }
}
