use pg_query::protobuf::ConstrType;
use pg_query::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddFkWithoutNotValid;

impl Rule for AddFkWithoutNotValid {
    fn id(&self) -> &'static str {
        "add-fk-without-not-valid"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for c in super::constraints_being_added(node) {
            if matches!(
                ConstrType::try_from(c.contype),
                Ok(ConstrType::ConstrForeign)
            ) && !c.skip_validation
            {
                out.push(RuleHit {
                    message: "Adding a FOREIGN KEY without NOT VALID validates every existing row \
                              while holding locks on both tables."
                        .into(),
                    guidance: "Add the constraint with NOT VALID first (brief lock, no scan), then run \
                               ALTER TABLE ... VALIDATE CONSTRAINT in a separate statement (it allows \
                               concurrent reads and writes)."
                        .into(),
                    fix: None,
                });
            }
        }

        // An inline FK (`... REFERENCES`) on an `ADD COLUMN` with a DEFAULT validates the default
        // against every existing row under lock (a nullable column with no default is exempt — NULL
        // is FK-exempt — so this is scoped to columns that carry a DEFAULT). The inline form cannot
        // take NOT VALID, so the safe rewrite differs from the constraint form.
        for col in super::columns_being_added(node) {
            if super::column_has_constraint(col, ConstrType::ConstrForeign)
                && super::column_has_constraint(col, ConstrType::ConstrDefault)
            {
                out.push(RuleHit {
                    message: "An inline FOREIGN KEY on a new column with a DEFAULT validates the \
                              default against every existing row while holding locks on both tables."
                        .into(),
                    guidance: "Add the column without the REFERENCES clause, then ADD CONSTRAINT ... \
                               FOREIGN KEY ... NOT VALID, then VALIDATE CONSTRAINT separately."
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
    fn flags_fk_without_not_valid() {
        let sql = "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id)";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        assert!(findings
            .iter()
            .any(|f| f.rule_id == "add-fk-without-not-valid"));
    }

    #[test]
    fn ignores_fk_with_not_valid() {
        let sql = "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id) NOT VALID";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        assert!(findings
            .iter()
            .all(|f| f.rule_id != "add-fk-without-not-valid"));
    }

    #[test]
    fn flags_inline_fk_on_add_column_with_default() {
        let sql = "ALTER TABLE t ADD COLUMN pid int DEFAULT 1 REFERENCES p (id)";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        assert!(findings
            .iter()
            .any(|f| f.rule_id == "add-fk-without-not-valid"));
    }

    #[test]
    fn ignores_inline_fk_on_add_column_without_default() {
        // No default — existing rows are NULL and NULL is FK-exempt, so no scan.
        let sql = "ALTER TABLE t ADD COLUMN pid int REFERENCES p (id)";
        let findings = lint_sql(sql, &LintOptions::default()).unwrap();
        assert!(findings
            .iter()
            .all(|f| f.rule_id != "add-fk-without-not-valid"));
    }
}
