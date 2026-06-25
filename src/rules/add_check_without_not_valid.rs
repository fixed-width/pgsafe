use pg_query::protobuf::ConstrType;
use pg_query::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct AddCheckWithoutNotValid;

impl Rule for AddCheckWithoutNotValid {
    fn id(&self) -> &'static str {
        "add-check-without-not-valid"
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
    }
}

#[cfg(test)]
mod tests {
    use crate::lint_sql;

    #[test]
    fn flags_check_without_not_valid() {
        let findings = lint_sql("ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0)").unwrap();
        assert!(findings
            .iter()
            .any(|f| f.rule_id == "add-check-without-not-valid"));
    }

    #[test]
    fn ignores_check_with_not_valid() {
        let findings = lint_sql("ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0) NOT VALID").unwrap();
        assert!(findings
            .iter()
            .all(|f| f.rule_id != "add-check-without-not-valid"));
    }
}
