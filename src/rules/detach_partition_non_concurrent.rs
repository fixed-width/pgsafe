use pg_query::protobuf::AlterTableType;
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct DetachPartitionNonConcurrent;

impl Rule for DetachPartitionNonConcurrent {
    fn id(&self) -> &'static str {
        "detach-partition-non-concurrent"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for cmd in super::alter_table_cmds(node) {
            if AlterTableType::try_from(cmd.subtype) != Ok(AlterTableType::AtDetachPartition) {
                continue;
            }
            // DETACH ... CONCURRENTLY (PartitionCmd.concurrent) is the safe form.
            let concurrent = matches!(
                cmd.def.as_ref().and_then(|n| n.node.as_ref()),
                Some(NodeEnum::PartitionCmd(pc)) if pc.concurrent
            );
            if !concurrent {
                out.push(RuleHit {
                    message: "DETACH PARTITION takes an ACCESS EXCLUSIVE lock on the parent table and \
                              the partition, blocking all access to the whole partitioned table while \
                              it runs."
                        .into(),
                    guidance: "Use ALTER TABLE ... DETACH PARTITION ... CONCURRENTLY (PostgreSQL 14+; it \
                               takes only SHARE UPDATE EXCLUSIVE on the parent, so reads and writes keep \
                               working). It must run outside a transaction block."
                        .into(),
                    fix: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions, Severity};

    fn findings(sql: &str) -> Vec<crate::Finding> {
        lint_sql(sql, &LintOptions::default()).unwrap()
    }

    #[test]
    fn flags_detach_partition() {
        assert!(findings("ALTER TABLE p DETACH PARTITION p1")
            .iter()
            .any(|f| f.rule_id == "detach-partition-non-concurrent"));
    }

    #[test]
    fn silent_on_detach_concurrently() {
        assert!(findings("ALTER TABLE p DETACH PARTITION p1 CONCURRENTLY")
            .iter()
            .all(|f| f.rule_id != "detach-partition-non-concurrent"));
    }

    #[test]
    fn silent_on_unrelated_alter() {
        assert!(findings("ALTER TABLE t ADD COLUMN c int")
            .iter()
            .all(|f| f.rule_id != "detach-partition-non-concurrent"));
    }

    #[test]
    fn detach_is_an_error() {
        let f = findings("ALTER TABLE p DETACH PARTITION p1")
            .into_iter()
            .find(|f| f.rule_id == "detach-partition-non-concurrent")
            .expect("rule must fire");
        assert_eq!(f.severity, Severity::Error);
    }
}
