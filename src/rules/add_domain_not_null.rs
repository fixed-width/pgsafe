use crate::ast::protobuf::ConstrType;
use crate::ast::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddDomainNotNull;

impl Rule for AddDomainNotNull {
    fn id(&self) -> &'static str {
        "add-domain-not-null"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        let NodeEnum::AlterDomainStmt(a) = node else {
            return;
        };
        // "C" = ADD CONSTRAINT (both `ADD NOT NULL` and `ADD CONSTRAINT c NOT NULL` use it, with a
        // `ConstrNotnull` def). A NOT NULL domain constraint cannot be added `NOT VALID`, so — unlike
        // the sibling `add-domain-constraint-without-not-valid` (CHECK) — there is no `skip_validation`
        // form to exempt and no autofix to offer.
        if a.subtype != "C" {
            return;
        }
        let Some(NodeEnum::Constraint(con)) = a.def.as_ref().and_then(|n| n.node.as_ref()) else {
            return;
        };
        if ConstrType::try_from(con.contype) == Ok(ConstrType::ConstrNotnull) {
            out.push(RuleHit {
                message: "ALTER DOMAIN ... ADD NOT NULL checks that no existing value of the domain \
                          type is null across every dependent table, scanning them and holding a lock \
                          that blocks writes for the scan's duration."
                    .into(),
                guidance: "A domain NOT NULL cannot be added NOT VALID (unlike a CHECK). Add it while \
                           the domain has few or no dependent rows, or drop the domain-level NOT NULL \
                           and instead set NOT NULL on each dependent column via the safe two-step \
                           (CHECK (col IS NOT NULL) NOT VALID, VALIDATE CONSTRAINT, then SET NOT NULL)."
                    .into(),
                fix: None,
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
            .any(|f| f.rule_id == "add-domain-not-null")
    }

    #[test]
    fn flags_add_not_null() {
        let f = findings("ALTER DOMAIN d ADD NOT NULL")
            .into_iter()
            .find(|f| f.rule_id == "add-domain-not-null")
            .expect("rule must fire");
        assert_eq!(f.severity, Severity::Error);
        assert!(
            f.fix.is_none(),
            "no autofix — domain NOT NULL can't be NOT VALID"
        );
    }

    #[test]
    fn flags_named_add_constraint_not_null() {
        assert!(fires("ALTER DOMAIN d ADD CONSTRAINT nn NOT NULL"));
    }

    #[test]
    fn ignores_add_check_constraint() {
        // The CHECK form is owned by add-domain-constraint-without-not-valid, not this rule.
        assert!(!fires("ALTER DOMAIN d ADD CONSTRAINT c CHECK (VALUE > 0)"));
    }

    #[test]
    fn ignores_drop_and_set_not_null() {
        // SET/DROP NOT NULL toggle the domain's own flag; they are not ADD CONSTRAINT (subtype "C").
        assert!(!fires("ALTER DOMAIN d SET NOT NULL"));
        assert!(!fires("ALTER DOMAIN d DROP NOT NULL"));
    }

    #[test]
    fn ignores_create_domain_not_null() {
        // A freshly created domain has no dependent columns, so its NOT NULL scans nothing (and it
        // is a CreateDomainStmt, not an AlterDomainStmt).
        assert!(!fires("CREATE DOMAIN d AS int NOT NULL"));
    }
}
