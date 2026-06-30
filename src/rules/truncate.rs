use pg_query::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct Truncate;

impl Rule for Truncate {
    fn id(&self) -> &'static str {
        "truncate"
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        if matches!(node, NodeEnum::TruncateStmt(_)) {
            out.push(RuleHit {
                message: "TRUNCATE takes an ACCESS EXCLUSIVE lock and irreversibly removes all rows; with \
                          CASCADE the lock propagates to every FK-referencing table."
                    .into(),
                guidance: "For ongoing data removal use a batched DELETE; reserve TRUNCATE for environments \
                           where downtime and data loss are acceptable."
                    .into(),
                fix: None,
            });
        }
    }
}
