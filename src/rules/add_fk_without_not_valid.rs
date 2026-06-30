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
    fn emits_a_not_valid_fix() {
        use crate::fix::apply;
        let sql = "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id);";
        let fs = lint_sql(sql, &LintOptions::default()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "add-fk-without-not-valid")
            .expect("rule must fire");
        let fix = f.fix.as_ref().expect("fix present");
        assert_eq!(fix.title, "Add NOT VALID");
        let fixed = apply(sql, fix);
        assert_eq!(
            fixed,
            "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id) NOT VALID;"
        );
        // Applying it clears the finding.
        assert!(lint_sql(&fixed, &LintOptions::default())
            .unwrap()
            .iter()
            .all(|f| f.rule_id != "add-fk-without-not-valid"));
    }

    #[test]
    fn emits_a_not_valid_fix_no_semicolon() {
        // Without a trailing semicolon StatementBodyEnd must still land at the true end.
        use crate::fix::apply;
        let sql = "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id)";
        let fs = lint_sql(sql, &LintOptions::default()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "add-fk-without-not-valid")
            .expect("rule must fire");
        let fix = f.fix.as_ref().expect("fix present");
        let fixed = apply(sql, fix);
        assert_eq!(
            fixed,
            "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id) NOT VALID"
        );
    }

    #[test]
    fn inline_fk_fix_is_none() {
        // Inline FK on ADD COLUMN cannot take NOT VALID — fix must be absent.
        let sql = "ALTER TABLE t ADD COLUMN pid int DEFAULT 1 REFERENCES p (id);";
        let fs = lint_sql(sql, &LintOptions::default()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "add-fk-without-not-valid")
            .expect("rule must fire");
        assert!(f.fix.is_none(), "inline FK must not produce a fix");
    }

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

    #[test]
    fn multi_command_fk_fix_is_none() {
        // Multiple commands in one ALTER TABLE — the fix is suppressed because StatementBodyEnd
        // is ambiguous when len > 1 (NOT VALID would bind to the wrong command).
        let sql =
            "ALTER TABLE t ADD COLUMN x int, ADD CONSTRAINT fk FOREIGN KEY (x) REFERENCES u (id);";
        let fs = lint_sql(sql, &LintOptions::default()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "add-fk-without-not-valid")
            .expect("rule must fire");
        assert!(
            f.fix.is_none(),
            "multi-command ALTER must not emit a fix (StatementBodyEnd ambiguous)"
        );
    }
}
