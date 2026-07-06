use crate::ast::protobuf::ConstrType;
use crate::ast::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddExclusionConstraint;

impl Rule for AddExclusionConstraint {
    fn id(&self) -> &'static str {
        "add-exclusion-constraint"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for c in super::constraints_being_added(node) {
            if matches!(
                ConstrType::try_from(c.contype),
                Ok(ConstrType::ConstrExclusion)
            ) {
                out.push(RuleHit {
                    message: "ALTER TABLE ... ADD CONSTRAINT ... EXCLUDE builds an index under an \
                              ACCESS EXCLUSIVE lock, scanning the whole table."
                        .into(),
                    guidance: "Adding an exclusion constraint locks the table while it builds the index. \
                               Add it during a low-traffic window; on a large table, weigh whether the \
                               constraint is necessary."
                        .into(),
                    fix: None,
                });
            }
        }
    }
}
