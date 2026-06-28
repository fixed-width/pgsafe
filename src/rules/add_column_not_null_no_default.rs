use pg_query::protobuf::{AlterTableType, ConstrType};
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddColumnNotNullNoDefault;

impl Rule for AddColumnNotNullNoDefault {
    fn id(&self) -> &'static str {
        "add-column-not-null-no-default"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for cmd in super::alter_table_cmds(node) {
            if !matches!(
                AlterTableType::try_from(cmd.subtype),
                Ok(AlterTableType::AtAddColumn)
            ) {
                continue;
            }
            let Some(NodeEnum::ColumnDef(col)) = cmd.def.as_ref().and_then(|d| d.node.as_ref())
            else {
                continue;
            };
            // A column-level PRIMARY KEY implies NOT NULL (the parser emits only `ConstrPrimary`, not a
            // separate `ConstrNotnull`), so `ADD COLUMN c int PRIMARY KEY` on a populated table fails
            // the same way as an explicit NOT NULL with no default.
            let has_not_null = col.constraints.iter().any(|cn| {
                matches!(cn.node.as_ref(), Some(NodeEnum::Constraint(con))
                if matches!(
                    ConstrType::try_from(con.contype),
                    Ok(ConstrType::ConstrNotnull) | Ok(ConstrType::ConstrPrimary)
                ))
            });
            let has_default = col.constraints.iter().any(|cn| {
                matches!(cn.node.as_ref(), Some(NodeEnum::Constraint(con))
                    if matches!(ConstrType::try_from(con.contype), Ok(ConstrType::ConstrDefault)))
            });
            if has_not_null && !has_default {
                out.push(RuleHit {
                    message: "ADD COLUMN ... NOT NULL with no DEFAULT fails immediately on any non-empty \
                              table — it cannot fill existing rows."
                        .into(),
                    guidance: "Add the column nullable, backfill in batches, then enforce NOT NULL via \
                               CHECK (col IS NOT NULL) NOT VALID + VALIDATE CONSTRAINT, then SET NOT NULL \
                               (PG12+ reuses the validated check and skips the scan)."
                        .into(),
                });
            }
        }
    }
}
