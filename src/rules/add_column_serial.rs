use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

/// The serial pseudo-types. Each is sugar for a sequence + `nextval` default +
/// NOT NULL, so adding one to a populated table rewrites every row.
const SERIAL_TYPES: &[&str] = &[
    "serial",
    "serial2",
    "serial4",
    "serial8",
    "smallserial",
    "bigserial",
];

pub struct AddColumnSerial;

impl Rule for AddColumnSerial {
    fn id(&self) -> &'static str {
        "add-column-serial"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for col in super::columns_being_added(node) {
            let Some(type_name) = col.type_name.as_ref() else {
                continue;
            };
            // Serial pseudo-types parse as a single, unqualified name; real types
            // are schema-qualified (e.g. ["pg_catalog", "int8"]).
            if type_name.names.len() != 1 {
                continue;
            }
            let is_serial = matches!(
                type_name.names.first().and_then(|n| n.node.as_ref()),
                Some(NodeEnum::String(s))
                    if SERIAL_TYPES.iter().any(|t| t.eq_ignore_ascii_case(&s.sval))
            );
            if is_serial {
                out.push(RuleHit {
                    message: "ADD COLUMN with a serial type (e.g. bigserial) creates a sequence and \
                              rewrites every existing row under an ACCESS EXCLUSIVE lock."
                        .into(),
                    guidance: "Add a plain nullable integer column (e.g. bigint), create the sequence \
                               and backfill existing rows in batches, then ALTER COLUMN ... SET DEFAULT \
                               nextval(...) and add NOT NULL via the safe two-step — do not add serial \
                               directly to a populated table."
                        .into(),
                    fix: None,
                });
            }
        }
    }
}
