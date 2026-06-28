use pg_query::protobuf::ConstrType;
use pg_query::NodeEnum;

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
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions};

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
