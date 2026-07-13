use crate::ast::protobuf::ConstrType;
use crate::ast::NodeEnum;

use super::Rule;
use crate::fix::{FixAnchor, FixDraft, FixDraftEdit};
use crate::{RuleHit, Severity};

pub struct AddDomainConstraintWithoutNotValid;

impl Rule for AddDomainConstraintWithoutNotValid {
    fn id(&self) -> &'static str {
        "add-domain-constraint-without-not-valid"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        let NodeEnum::AlterDomainStmt(a) = node else {
            return;
        };
        // `AlterDomainStmt.subtype` is a single-char code; "C" = ADD CONSTRAINT (verified against
        // the parser). VALIDATE CONSTRAINT ("V") and DROP CONSTRAINT ("X") are deliberately not
        // flagged: VALIDATE is the sanctioned safe follow-up this rule's own fix points to.
        if a.subtype != "C" {
            return;
        }
        let Some(NodeEnum::Constraint(con)) = a.def.as_ref().and_then(|n| n.node.as_ref()) else {
            return;
        };
        // Domain ADD CONSTRAINT is CHECK-only; guard the contype anyway. `NOT VALID` sets
        // `skip_validation`, which defers the scan — exactly the safe form we want.
        if ConstrType::try_from(con.contype) == Ok(ConstrType::ConstrCheck) && !con.skip_validation
        {
            out.push(RuleHit {
                message: "ALTER DOMAIN ... ADD CONSTRAINT without NOT VALID validates the new CHECK \
                          against every existing value of the domain type across all dependent tables, \
                          scanning and locking them."
                    .into(),
                guidance: "Add the constraint with NOT VALID, then run ALTER DOMAIN ... VALIDATE \
                           CONSTRAINT separately."
                    .into(),
                // An ALTER DOMAIN statement carries exactly one action, so StatementBodyEnd is
                // unambiguous (unlike a multi-command ALTER TABLE) — the fix is always safe to offer.
                fix: Some(FixDraft {
                    title: "Add NOT VALID",
                    edits: vec![FixDraftEdit {
                        anchor: FixAnchor::StatementBodyEnd,
                        replacement: " NOT VALID".into(),
                    }],
                }),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions, Severity};

    fn findings(sql: &str) -> Vec<crate::Finding> {
        lint_sql(sql, &LintOptions::default()).unwrap()
    }
    fn fires(sql: &str) -> bool {
        findings(sql)
            .iter()
            .any(|f| f.rule_id == "add-domain-constraint-without-not-valid")
    }

    #[test]
    fn flags_add_constraint_without_not_valid() {
        let f = findings("ALTER DOMAIN us_postal ADD CONSTRAINT fmt CHECK (VALUE ~ '^[0-9]{5}$')")
            .into_iter()
            .find(|f| f.rule_id == "add-domain-constraint-without-not-valid")
            .expect("rule must fire");
        assert_eq!(f.severity, Severity::Error);
    }

    #[test]
    fn ignores_add_constraint_with_not_valid() {
        assert!(!fires(
            "ALTER DOMAIN us_postal ADD CONSTRAINT fmt CHECK (VALUE ~ '^[0-9]{5}$') NOT VALID"
        ));
    }

    #[test]
    fn ignores_create_domain_with_check() {
        // A freshly created domain has no dependent columns, so its CHECK locks nothing.
        assert!(!fires(
            "CREATE DOMAIN us_postal AS text CHECK (VALUE ~ '^[0-9]{5}$')"
        ));
    }

    #[test]
    fn ignores_validate_constraint() {
        // VALIDATE CONSTRAINT is the sanctioned safe follow-up; do not flag it.
        assert!(!fires("ALTER DOMAIN us_postal VALIDATE CONSTRAINT fmt"));
    }

    #[test]
    fn ignores_drop_constraint() {
        assert!(!fires("ALTER DOMAIN us_postal DROP CONSTRAINT fmt"));
    }

    #[test]
    fn emits_not_valid_fix_that_clears() {
        use crate::fix::apply;
        let sql = "ALTER DOMAIN us_postal ADD CONSTRAINT fmt CHECK (VALUE > 0);";
        let f = findings(sql)
            .into_iter()
            .find(|f| f.rule_id == "add-domain-constraint-without-not-valid")
            .unwrap();
        let fix = f.fix.as_ref().expect("fix present");
        assert_eq!(fix.title, "Add NOT VALID");
        let fixed = apply(sql, &fix.edits);
        assert_eq!(
            fixed,
            "ALTER DOMAIN us_postal ADD CONSTRAINT fmt CHECK (VALUE > 0) NOT VALID;"
        );
        assert!(
            !lint_sql(&fixed, &LintOptions::default())
                .unwrap()
                .iter()
                .any(|f| f.rule_id == "add-domain-constraint-without-not-valid"),
            "fixed SQL must not re-trigger the rule"
        );
    }
}
