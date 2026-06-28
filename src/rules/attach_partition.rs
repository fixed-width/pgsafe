use pg_query::protobuf::AlterTableType;
use pg_query::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct AttachPartition;

impl Rule for AttachPartition {
    fn id(&self) -> &'static str {
        "attach-partition"
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for cmd in super::alter_table_cmds(node) {
            if matches!(
                AlterTableType::try_from(cmd.subtype),
                Ok(AlterTableType::AtAttachPartition)
            ) {
                out.push(RuleHit {
                    message: "ATTACH PARTITION takes an ACCESS EXCLUSIVE lock on the table being \
                              attached and scans it to validate the partition bound (the parent stays \
                              available under SHARE UPDATE EXCLUSIVE), so the table being attached is \
                              unavailable for the scan's duration."
                        .into(),
                    guidance: "Add a CHECK constraint on the child matching the partition bound and \
                               validate it separately first (ADD CONSTRAINT ... CHECK (...) NOT VALID, \
                               then VALIDATE CONSTRAINT); ATTACH then skips the scan and the lock is \
                               brief."
                        .into(),
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

    const ATTACH: &str = "ALTER TABLE parent ATTACH PARTITION child FOR VALUES FROM (0) TO (100)";

    #[test]
    fn flags_attach_partition() {
        assert!(findings(ATTACH)
            .iter()
            .any(|f| f.rule_id == "attach-partition"));
    }

    #[test]
    fn attach_is_a_warning() {
        let f = findings(ATTACH)
            .into_iter()
            .find(|f| f.rule_id == "attach-partition")
            .expect("rule must fire");
        assert_eq!(f.severity, Severity::Warning);
    }

    #[test]
    fn silent_on_unrelated_alter() {
        assert!(findings("ALTER TABLE t ADD COLUMN c int")
            .iter()
            .all(|f| f.rule_id != "attach-partition"));
    }

    #[test]
    fn silent_on_attach_of_same_migration_empty_child() {
        // child is created empty in the same migration → no validation scan → exempt.
        let sql = format!("CREATE TABLE child (id int); {ATTACH};");
        assert!(findings(&sql)
            .iter()
            .all(|f| f.rule_id != "attach-partition"));
    }

    #[test]
    fn fires_on_attach_of_populated_child() {
        // populating the child removes the empty exemption.
        let sql = format!("CREATE TABLE child (id int); INSERT INTO child VALUES (1); {ATTACH};");
        assert!(findings(&sql)
            .iter()
            .any(|f| f.rule_id == "attach-partition"));
    }
}
