use crate::ast::protobuf::ConstrType;
use crate::ast::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddCheckWithoutNotValid;

impl Rule for AddCheckWithoutNotValid {
    fn id(&self) -> &'static str {
        "add-check-without-not-valid"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for c in super::constraints_being_added(node) {
            if matches!(ConstrType::try_from(c.contype), Ok(ConstrType::ConstrCheck))
                && !c.skip_validation
            {
                out.push(RuleHit {
                    message: "Adding a CHECK constraint without NOT VALID scans the whole table \
                              under an ACCESS EXCLUSIVE lock."
                        .into(),
                    guidance:
                        "Add the CHECK with NOT VALID, then run VALIDATE CONSTRAINT separately \
                               (SHARE UPDATE EXCLUSIVE — concurrent reads and writes are allowed)."
                            .into(),
                    // Fix is only safe when this is the sole ALTER TABLE command; with multiple
                    // commands StatementBodyEnd is ambiguous — NOT VALID would bind incorrectly.
                    fix: (super::alter_table_cmds(node).len() == 1).then(|| crate::fix::FixDraft {
                        title: "Add NOT VALID",
                        edits: vec![crate::fix::FixDraftEdit {
                            anchor: crate::fix::FixAnchor::StatementBodyEnd,
                            replacement: " NOT VALID".into(),
                        }],
                    }),
                });
            }
        }

        // An inline CHECK on an `ADD COLUMN` scans the whole table to validate the constraint under
        // an ACCESS EXCLUSIVE lock — regardless of any DEFAULT, and even with no default (NULL rows
        // are still scanned; measured ~320ms-1s on 10M rows vs <1ms for a plain ADD COLUMN). The
        // inline form cannot take NOT VALID, so the safe rewrite differs from the constraint form.
        // (A same-migration empty table is exempted separately by the new-table tracker.)
        for col in super::columns_being_added(node) {
            if super::column_has_constraint(col, ConstrType::ConstrCheck) {
                out.push(RuleHit {
                    message: "An inline CHECK on a new column scans the whole table to validate it \
                              under an ACCESS EXCLUSIVE lock."
                        .into(),
                    guidance: "Add the column without the CHECK, then ADD CONSTRAINT ... CHECK (...) \
                               NOT VALID, then VALIDATE CONSTRAINT separately (SHARE UPDATE EXCLUSIVE)."
                        .into(),
                    fix: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions};

    #[test]
    fn emits_a_not_valid_fix() {
        use crate::fix::apply;
        let sql = "ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0);";
        let fs = lint_sql(sql, &LintOptions::default()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "add-check-without-not-valid")
            .expect("rule must fire");
        let fix = f.fix.as_ref().expect("fix present");
        assert_eq!(fix.title, "Add NOT VALID");
        let fixed = apply(sql, &fix.edits);
        assert_eq!(
            fixed,
            "ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0) NOT VALID;"
        );
        // Applying it clears the finding.
        assert!(lint_sql(&fixed, &LintOptions::default())
            .unwrap()
            .iter()
            .all(|f| f.rule_id != "add-check-without-not-valid"));
    }

    #[test]
    fn multi_cmd_alter_constraint_fix_is_none() {
        // Two commands: ADD COLUMN + ADD CONSTRAINT — NOT VALID would be ambiguous at
        // StatementBodyEnd, so the fix must be None.
        let sql = "ALTER TABLE t ADD COLUMN x int, ADD CONSTRAINT ck CHECK (a > 0);";
        let fs = lint_sql(sql, &LintOptions::default()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "add-check-without-not-valid")
            .expect("rule must fire");
        assert!(
            f.fix.is_none(),
            "multi-cmd ALTER TABLE must not produce a fix"
        );
    }

    #[test]
    fn flags_check_without_not_valid() {
        let findings = lint_sql(
            "ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0)",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings
            .iter()
            .any(|f| f.rule_id == "add-check-without-not-valid"));
    }

    #[test]
    fn ignores_check_with_not_valid() {
        let findings = lint_sql(
            "ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0) NOT VALID",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings
            .iter()
            .all(|f| f.rule_id != "add-check-without-not-valid"));
    }

    #[test]
    fn flags_inline_check_on_add_column_with_default() {
        let findings = lint_sql(
            "ALTER TABLE t ADD COLUMN x int DEFAULT 1 CHECK (x > 0)",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings
            .iter()
            .any(|f| f.rule_id == "add-check-without-not-valid"));
    }

    #[test]
    fn flags_inline_check_on_add_column_without_default() {
        // Even with no default, the inline CHECK is validated by scanning the table (the NULL rows
        // are still scanned) under ACCESS EXCLUSIVE — proven by tests/rule_proofs.rs (seq_scan++).
        let findings = lint_sql(
            "ALTER TABLE t ADD COLUMN x int CHECK (x > 0)",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings
            .iter()
            .any(|f| f.rule_id == "add-check-without-not-valid"));
    }
}
