use pg_query::protobuf::ConstrType;
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddUniqueConstraint;

impl Rule for AddUniqueConstraint {
    fn id(&self) -> &'static str {
        "add-unique-constraint"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for c in super::constraints_being_added(node) {
            if matches!(
                ConstrType::try_from(c.contype),
                Ok(ConstrType::ConstrUnique)
            ) && c.indexname.is_empty()
            {
                out.push(RuleHit {
                    message: "Adding a UNIQUE constraint inline builds its underlying index while holding \
                              ACCESS EXCLUSIVE on the table for the whole build."
                        .into(),
                    guidance: "Build the index first with CREATE UNIQUE INDEX CONCURRENTLY, then attach it: \
                               ALTER TABLE ... ADD CONSTRAINT ... UNIQUE USING INDEX idx (a brief lock)."
                        .into(),
                });
            }
        }
    }
}
