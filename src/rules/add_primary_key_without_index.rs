use pg_query::protobuf::{AlterTableType, ConstrType};
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddPrimaryKeyWithoutIndex;

impl Rule for AddPrimaryKeyWithoutIndex {
    fn id(&self) -> &'static str {
        "add-primary-key-without-index"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        let via_constraint = super::constraints_being_added(node).into_iter().any(|c| {
            matches!(
                ConstrType::try_from(c.contype),
                Ok(ConstrType::ConstrPrimary)
            ) && c.indexname.is_empty()
        });
        let via_column = super::alter_table_cmds(node).into_iter().any(|cmd| {
            if !matches!(
                AlterTableType::try_from(cmd.subtype),
                Ok(AlterTableType::AtAddColumn)
            ) {
                return false;
            }
            let Some(NodeEnum::ColumnDef(col)) = cmd.def.as_ref().and_then(|d| d.node.as_ref())
            else {
                return false;
            };
            col.constraints.iter().any(|cn| {
                matches!(cn.node.as_ref(), Some(NodeEnum::Constraint(con))
                    if matches!(ConstrType::try_from(con.contype), Ok(ConstrType::ConstrPrimary)))
            })
        });
        if via_constraint || via_column {
            out.push(RuleHit {
                message: "Adding a PRIMARY KEY inline builds its unique index (and may scan for NOT NULL) \
                          under an ACCESS EXCLUSIVE lock."
                    .into(),
                guidance: "Build the index with CREATE UNIQUE INDEX CONCURRENTLY, then attach it: \
                           ALTER TABLE ... ADD CONSTRAINT ... PRIMARY KEY USING INDEX idx."
                    .into(),
            });
        }
    }
}
