use crate::ast::protobuf::ConstrType;
use crate::ast::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct AddDomainNotNull;

impl Rule for AddDomainNotNull {
    fn id(&self) -> &'static str {
        "add-domain-not-null"
    }
    // Error, per the documented criterion (a blocking scan-lock on live, in-service dependent tables
    // + a well-known safe alternative). There is no NOT VALID for a domain NOT NULL, so the safe
    // path is the column-level redesign in `guidance` (a redesign counts as the safe rewrite).
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        let NodeEnum::AlterDomainStmt(a) = node else {
            return;
        };
        // Establishing a NOT NULL on an existing domain — either `ADD [CONSTRAINT] NOT NULL`
        // (subtype "C" with a `ConstrNotnull` def; PG17+) or `SET NOT NULL` (subtype "O", no def;
        // the portable spelling, all supported versions). `DROP NOT NULL` (subtype "N") relaxes the
        // constraint — no scan, brief lock — and is not flagged.
        let establishes_not_null = match a.subtype.as_str() {
            "O" => true,
            "C" => matches!(
                a.def.as_ref().and_then(|n| n.node.as_ref()),
                Some(NodeEnum::Constraint(con))
                    if ConstrType::try_from(con.contype) == Ok(ConstrType::ConstrNotnull)
            ),
            _ => false,
        };
        if establishes_not_null {
            out.push(RuleHit {
                message: "ALTER DOMAIN ... ADD/SET NOT NULL checks that no existing value of the \
                          domain type is null across every dependent table, scanning them and holding \
                          a lock that blocks writes for the scan's duration."
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
    fn flags_set_not_null() {
        // SET NOT NULL (subtype "O") is the portable spelling and carries the same scan/lock hazard.
        assert!(fires("ALTER DOMAIN d SET NOT NULL"));
    }

    #[test]
    fn ignores_add_check_constraint() {
        // The CHECK form is owned by add-domain-constraint-without-not-valid, not this rule.
        assert!(!fires("ALTER DOMAIN d ADD CONSTRAINT c CHECK (VALUE > 0)"));
    }

    #[test]
    fn ignores_drop_not_null() {
        // DROP NOT NULL relaxes the constraint: no scan, brief lock — genuinely safe.
        assert!(!fires("ALTER DOMAIN d DROP NOT NULL"));
    }

    #[test]
    fn ignores_create_domain_not_null() {
        // A freshly created domain has no dependent columns, so its NOT NULL scans nothing (and it
        // is a CreateDomainStmt, not an AlterDomainStmt).
        assert!(!fires("CREATE DOMAIN d AS int NOT NULL"));
    }
}
