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

        // An inline CHECK on an `ADD COLUMN` with a DEFAULT validates the default against every
        // existing row under the lock (a nullable column with no default is exempt — existing rows
        // are NULL and a CHECK passes on NULL — so this is scoped to columns that carry a DEFAULT).
        // The inline form cannot take NOT VALID, so the safe rewrite differs from the constraint form.
        for col in super::columns_being_added(node) {
            if super::column_has_constraint(col, ConstrType::ConstrCheck)
                && super::column_has_constraint(col, ConstrType::ConstrDefault)
            {
                out.push(RuleHit {
                    message: "An inline CHECK on a new column with a DEFAULT validates the default \
                              against every existing row under an ACCESS EXCLUSIVE lock."
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
    fn ignores_inline_check_on_add_column_without_default() {
        // No default — existing rows are NULL and a CHECK passes on NULL, so no scan.
        let findings = lint_sql(
            "ALTER TABLE t ADD COLUMN x int CHECK (x > 0)",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(findings
            .iter()
            .all(|f| f.rule_id != "add-check-without-not-valid"));
    }
}
