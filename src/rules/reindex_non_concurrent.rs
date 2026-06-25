use pg_query::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct ReindexNonConcurrent;

impl Rule for ReindexNonConcurrent {
    fn id(&self) -> &'static str {
        "reindex-non-concurrent"
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        if let NodeEnum::ReindexStmt(r) = node {
            let concurrent = r.params.iter().any(|p| {
                matches!(p.node.as_ref(), Some(NodeEnum::DefElem(de)) if de.defname == "concurrently")
            });
            if !concurrent {
                out.push(RuleHit {
                    message: "REINDEX without CONCURRENTLY takes an ACCESS EXCLUSIVE lock on each index it \
                              rebuilds, blocking writes (and reads through that index)."
                        .into(),
                    guidance: "Use REINDEX INDEX CONCURRENTLY (PG12+, outside a transaction); on older \
                               servers use pg_repack or a maintenance window."
                        .into(),
                });
            }
        }
    }
}
