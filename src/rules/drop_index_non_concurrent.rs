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
                });
            }
        }
    }
}
