use pg_query::protobuf::ConstrType;
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddColumnGeneratedStored;

impl Rule for AddColumnGeneratedStored {
    fn id(&self) -> &'static str {
        "add-column-generated-stored"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for col in super::columns_being_added(node) {
            // GENERATED ALWAYS AS (...) STORED produces a ConstrGenerated constraint;
            // GENERATED ... AS IDENTITY is ConstrIdentity and does not match.
            if super::column_has_constraint(col, ConstrType::ConstrGenerated) {
                out.push(RuleHit {
                    message: "ADD COLUMN ... GENERATED ALWAYS AS (...) STORED computes and writes the \
                              value for every existing row, rewriting the table under an ACCESS \
                              EXCLUSIVE lock."
                        .into(),
                    guidance: "Add a plain nullable column, backfill the computed value in batches, \
                               and keep it current with a trigger or in application code — do not add \
                               a STORED generated column directly to a populated table."
                        .into(),
                    fix: None,
                });
            }
        }
    }
}
