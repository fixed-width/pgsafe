use crate::ast::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct AddTrigger;

impl Rule for AddTrigger {
    fn id(&self) -> &'static str {
        "add-trigger"
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        if matches!(node, NodeEnum::CreateTrigStmt(_)) {
            out.push(RuleHit {
                message: "CREATE TRIGGER takes a SHARE ROW EXCLUSIVE lock (blocking writes and other \
                          DDL, though not reads) and changes behavior for every subsequent write to \
                          the table."
                    .into(),
                guidance: "Create the trigger during a low-traffic window; its lock conflicts with \
                           concurrent writes on the table."
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

    fn trigger_sql(table: &str) -> String {
        format!("CREATE TRIGGER trg AFTER INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION f()")
    }

    #[test]
    fn flags_create_trigger() {
        assert!(findings(&trigger_sql("t"))
            .iter()
            .any(|f| f.rule_id == "add-trigger"));
    }

    #[test]
    fn add_trigger_is_a_warning() {
        let f = findings(&trigger_sql("t"))
            .into_iter()
            .find(|f| f.rule_id == "add-trigger")
            .expect("rule must fire");
        assert_eq!(f.severity, Severity::Warning);
    }

    #[test]
    fn silent_on_non_trigger_statements() {
        assert!(findings("SELECT 1")
            .iter()
            .all(|f| f.rule_id != "add-trigger"));
    }

    #[test]
    fn silent_on_trigger_for_same_migration_new_table() {
        // foo is created (empty) earlier in the same input → the trigger is exempt.
        let sql = format!("CREATE TABLE foo (id int); {}", trigger_sql("foo"));
        assert!(findings(&sql).iter().all(|f| f.rule_id != "add-trigger"));
    }
}
