use pg_query::protobuf::AlterTableType;
use pg_query::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct DropColumn;

impl Rule for DropColumn {
    fn id(&self) -> &'static str {
        "drop-column"
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for cmd in super::alter_table_cmds(node) {
            if matches!(
                AlterTableType::try_from(cmd.subtype),
                Ok(AlterTableType::AtDropColumn)
            ) {
                out.push(RuleHit {
                    message:
                        "DROP COLUMN breaks any application code still referencing the column the \
                              moment it runs."
                            .into(),
                    guidance:
                        "Use expand/contract: deploy code that stops using the column first, then \
                               drop it in a later migration."
                            .into(),
                    fix: None,
                });
            }
        }
    }
}
