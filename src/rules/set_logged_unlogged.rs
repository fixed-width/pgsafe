use pg_query::protobuf::AlterTableType;
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct SetLoggedUnlogged;

impl Rule for SetLoggedUnlogged {
    fn id(&self) -> &'static str {
        "set-logged-unlogged"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for cmd in super::alter_table_cmds(node) {
            if matches!(
                AlterTableType::try_from(cmd.subtype),
                Ok(AlterTableType::AtSetLogged | AlterTableType::AtSetUnLogged)
            ) {
                out.push(RuleHit {
                    message:
                        "ALTER TABLE ... SET LOGGED/UNLOGGED rewrites the entire table and its \
                              indexes under an ACCESS EXCLUSIVE lock."
                            .into(),
                    guidance:
                        "There is no online alternative — toggling durability rewrites the table. \
                               Do it in a maintenance window, and avoid it on a large live table."
                            .into(),
                    fix: None,
                });
            }
        }
    }
}
