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
                // Fix is only valid for a single-command ALTER TABLE; a multi-command
                // statement needs manual rewrite since StatementBodyEnd is ambiguous.
                out.push(RuleHit {
                    message: "DETACH PARTITION takes an ACCESS EXCLUSIVE lock on the parent table and \
                              the partition, blocking all access to the whole partitioned table while \
                              it runs."
                        .into(),
                    guidance: "Use ALTER TABLE ... DETACH PARTITION ... CONCURRENTLY (PostgreSQL 14+; it \
                               takes only SHARE UPDATE EXCLUSIVE on the parent, so reads and writes keep \
                               working). It must run outside a transaction block."
                        .into(),
                    fix: (super::alter_table_cmds(node).len() == 1).then(|| crate::fix::FixDraft {
                        title: "Add CONCURRENTLY",
                        edits: vec![crate::fix::FixDraftEdit {
                            anchor: crate::fix::FixAnchor::StatementBodyEnd,
                            replacement: " CONCURRENTLY".into(),
                        }],
                    }),
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

    #[test]
    fn multi_cmd_alter_has_no_fix() {
        // PostgreSQL grammar prevents DETACH PARTITION from being combined with
        // other ALTER TABLE sub-commands in the same statement, so we synthesise a
        // two-command AlterTableStmt directly to exercise the single-command gate.
        use super::super::Rule as _;
        use pg_query::protobuf::{
            AlterTableCmd, AlterTableStmt, AlterTableType, Node, PartitionCmd, RangeVar,
        };
        use pg_query::NodeEnum;

        let detach_cmd = AlterTableCmd {
            subtype: AlterTableType::AtDetachPartition as i32,
            def: Some(Box::new(Node {
                node: Some(NodeEnum::PartitionCmd(PartitionCmd {
                    name: Some(RangeVar {
                        relname: "p1".into(),
                        ..Default::default()
                    }),
                    concurrent: false,
                    ..Default::default()
                })),
            })),
            ..Default::default()
        };
        let other_cmd = AlterTableCmd {
            subtype: AlterTableType::AtDropColumn as i32,
            name: "c".into(),
            ..Default::default()
        };
        let node = NodeEnum::AlterTableStmt(AlterTableStmt {
            relation: Some(RangeVar {
                relname: "p".into(),
                ..Default::default()
            }),
            cmds: vec![
                Node {
                    node: Some(NodeEnum::AlterTableCmd(Box::new(detach_cmd))),
                },
                Node {
                    node: Some(NodeEnum::AlterTableCmd(Box::new(other_cmd))),
                },
            ],
            ..Default::default()
        });

        let mut out = Vec::new();
        super::DetachPartitionNonConcurrent.check(&node, &mut out);
        let h = out.first().expect("rule must fire");
        assert!(
            h.fix.is_none(),
            "multi-cmd ALTER TABLE must not produce a fix"
        );
    }

    #[test]
    fn emits_a_concurrently_fix() {
        use crate::fix::apply;
        let sql = "ALTER TABLE p DETACH PARTITION p1;";
        let fs = findings(sql);
        let f = fs
            .iter()
            .find(|f| f.rule_id == "detach-partition-non-concurrent")
            .unwrap();
        let fix = f.fix.as_ref().expect("fix present");
        assert_eq!(fix.title, "Add CONCURRENTLY");
        let fixed = apply(sql, fix);
        assert_eq!(fixed, "ALTER TABLE p DETACH PARTITION p1 CONCURRENTLY;");
        // Applying it clears the finding.
        assert!(findings(&fixed)
            .iter()
            .all(|f| f.rule_id != "detach-partition-non-concurrent"));
    }
}
