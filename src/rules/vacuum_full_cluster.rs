use crate::ast::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct VacuumFullOrCluster;

impl Rule for VacuumFullOrCluster {
    fn id(&self) -> &'static str {
        "vacuum-full-cluster"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        let fires = match node {
            NodeEnum::VacuumStmt(v) => {
                v.is_vacuumcmd
                    && v.options.iter().any(|opt| {
                        matches!(opt.node.as_ref(), Some(NodeEnum::DefElem(de))
                            if de.defname == "full" && super::defelem_is_true(de))
                    })
            }
            NodeEnum::ClusterStmt(_) => true,
            _ => false,
        };
        if fires {
            out.push(RuleHit {
                message: "VACUUM FULL and CLUSTER rewrite the entire table under an ACCESS EXCLUSIVE lock — \
                          minutes to hours of blocked reads and writes, plus 2x disk."
                    .into(),
                guidance: "On PostgreSQL 19+, rebuild online with REPACK (CONCURRENTLY): it takes \
                           ACCESS EXCLUSIVE only briefly to swap files while allowing concurrent reads \
                           and writes (requires a primary key; not for partitioned or unlogged tables, \
                           and must run outside a transaction block). On earlier versions use the \
                           pg_repack extension. To only reclaim bloat, plain VACUUM (no FULL) takes just \
                           SHARE UPDATE EXCLUSIVE and allows concurrent reads and writes."
                    .into(),
                fix: None,
            });
        }
    }
}
