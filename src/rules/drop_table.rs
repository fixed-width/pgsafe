use pg_query::protobuf::ObjectType;
use pg_query::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct DropTable;

impl Rule for DropTable {
    fn id(&self) -> &'static str {
        "drop-table"
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        if let NodeEnum::DropStmt(d) = node {
            if matches!(
                ObjectType::try_from(d.remove_type),
                Ok(ObjectType::ObjectTable)
            ) {
                out.push(RuleHit {
                    message: "DROP TABLE permanently and irreversibly removes the table and all its data; \
                              in-flight queries against it fail immediately."
                        .into(),
                    guidance: "Confirm all application references are retired and the table is traffic-free \
                               before dropping; archive the data first if it may be needed."
                        .into(),
                    fix: None,
                });
            }
        }
    }
}
