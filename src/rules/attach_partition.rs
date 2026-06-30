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

    fn findings_with_override(sql: &str, sev: Severity) -> Vec<crate::Finding> {
        let mut opts = LintOptions::default();
        opts.severity_overrides
            .insert("attach-partition".to_string(), sev);
        lint_sql(sql, &opts).unwrap()
    }

    fn attach_finding(fs: Vec<crate::Finding>) -> crate::Finding {
        fs.into_iter()
            .find(|f| f.rule_id == "attach-partition")
            .expect("attach-partition rule must fire")
    }

    const ATTACH: &str = "ALTER TABLE parent ATTACH PARTITION child FOR VALUES FROM (0) TO (100)";

    #[test]
    fn flags_attach_partition() {
        assert!(findings(ATTACH)
            .iter()
            .any(|f| f.rule_id == "attach-partition"));
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

    #[test]
    fn pre_existing_child_is_error() {
        // Child never created in this migration -> may be a live table -> Error.
        assert_eq!(attach_finding(findings(ATTACH)).severity, Severity::Error);
    }

    #[test]
    fn pre_existing_child_error_explains_why() {
        // The escalated finding appends the pre-existing-child explanation.
        assert!(attach_finding(findings(ATTACH))
            .message
            .contains("not created in this migration"));
    }

    #[test]
    fn not_valid_check_then_validate_keeps_warning() {
        let sql = format!(
            "ALTER TABLE child ADD CONSTRAINT cc CHECK (id >= 0 AND id < 100) NOT VALID; \
             ALTER TABLE child VALIDATE CONSTRAINT cc; {ATTACH};"
        );
        assert_eq!(attach_finding(findings(&sql)).severity, Severity::Warning);
    }

    #[test]
    fn plain_check_keeps_warning() {
        let sql =
            format!("ALTER TABLE child ADD CONSTRAINT cc CHECK (id >= 0 AND id < 100); {ATTACH};");
        assert_eq!(attach_finding(findings(&sql)).severity, Severity::Warning);
    }

    #[test]
    fn populated_same_migration_child_is_warning() {
        // Child built (and filled) in this migration is not in service -> stays Warning.
        let sql = format!("CREATE TABLE child (id int); INSERT INTO child VALUES (1); {ATTACH};");
        assert_eq!(attach_finding(findings(&sql)).severity, Severity::Warning);
    }

    #[test]
    fn explicit_warning_override_beats_escalation() {
        // A user who forces attach-partition to warning keeps warning even on the pre-existing case.
        assert_eq!(
            attach_finding(findings_with_override(ATTACH, Severity::Warning)).severity,
            Severity::Warning
        );
    }

    #[test]
    fn explicit_error_override_applies_to_non_escalated_case() {
        // A user who forces attach-partition to error gets error even on the same-migration case.
        let sql = format!("CREATE TABLE child (id int); INSERT INTO child VALUES (1); {ATTACH};");
        assert_eq!(
            attach_finding(findings_with_override(&sql, Severity::Error)).severity,
            Severity::Error
        );
    }

    #[test]
    fn not_valid_check_without_validate_is_error() {
        // ADD CHECK ... NOT VALID with no following VALIDATE does not prepare the child;
        // ATTACH still runs the full validation scan -> Error. (The primary anti-pattern.)
        let sql = format!(
            "ALTER TABLE child ADD CONSTRAINT cc CHECK (id >= 0 AND id < 100) NOT VALID; {ATTACH};"
        );
        assert_eq!(attach_finding(findings(&sql)).severity, Severity::Error);
    }

    #[test]
    fn validate_on_other_table_does_not_lift_escalation() {
        // The VALIDATE targets a different table, so child's NOT VALID check stays unvalidated -> Error.
        let sql = format!(
            "ALTER TABLE child ADD CONSTRAINT cc CHECK (id >= 0 AND id < 100) NOT VALID; \
             ALTER TABLE other VALIDATE CONSTRAINT cc; {ATTACH};"
        );
        assert_eq!(attach_finding(findings(&sql)).severity, Severity::Error);
    }

    #[test]
    fn escalation_is_per_attach_selective() {
        // Two ATTACHes in one migration: pre-existing child_a escalates (Error); same-migration
        // populated child_b stays Warning. Escalation is per-statement, not all-or-nothing.
        let sql = "CREATE TABLE child_b (id int); INSERT INTO child_b VALUES (1); \
                   ALTER TABLE pa ATTACH PARTITION child_a FOR VALUES FROM (0) TO (100); \
                   ALTER TABLE pb ATTACH PARTITION child_b FOR VALUES FROM (100) TO (200);";
        let fs = findings(sql);
        let sev = |child: &str| {
            fs.iter()
                .find(|f| f.rule_id == "attach-partition" && f.snippet.contains(child))
                .unwrap_or_else(|| panic!("no attach finding for {child}"))
                .severity
        };
        assert_eq!(sev("child_a"), Severity::Error);
        assert_eq!(sev("child_b"), Severity::Warning);
    }
}
