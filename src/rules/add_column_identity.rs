use pg_query::protobuf::ConstrType;
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddColumnIdentity;

impl Rule for AddColumnIdentity {
    fn id(&self) -> &'static str {
        "add-column-identity"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for col in super::columns_being_added(node) {
            // Both GENERATED ALWAYS / BY DEFAULT AS IDENTITY produce a ConstrIdentity
            // constraint; GENERATED ... STORED is ConstrGenerated and does not match.
            if super::column_has_constraint(col, ConstrType::ConstrIdentity) {
                out.push(RuleHit {
                    message:
                        "ADD COLUMN ... GENERATED AS IDENTITY creates a sequence and rewrites \
                              every existing row under an ACCESS EXCLUSIVE lock."
                            .into(),
                    guidance:
                        "Add a plain nullable integer column, backfill existing rows in batches, \
                               then attach the identity/sequence — do not add an identity column \
                               directly to a populated table."
                            .into(),
                    fix: None,
                });
            }
        }
    }
}
