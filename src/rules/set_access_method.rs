use crate::ast::protobuf::AlterTableType;
use crate::ast::NodeEnum;

use super::Rule;
use crate::{RuleHit, Severity};

pub struct SetAccessMethod;

impl Rule for SetAccessMethod {
    fn id(&self) -> &'static str {
        "set-access-method"
    }
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for cmd in super::alter_table_cmds(node) {
            if matches!(
                AlterTableType::try_from(cmd.subtype),
                Ok(AlterTableType::AtSetAccessMethod)
            ) {
                out.push(RuleHit {
                    message: "ALTER TABLE ... SET ACCESS METHOD rewrites the entire table and rebuilds \
                              its indexes under an ACCESS EXCLUSIVE lock when the access method changes, \
                              blocking all reads and writes for the rewrite."
                        .into(),
                    guidance: "There is no online way to change a table's access method. Do it in a \
                               maintenance window, or create a new table with the target access method, \
                               copy the data in batches, then swap (expand/contract)."
                        .into(),
                    fix: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions, Severity};

    fn findings(sql: &str) -> Vec<crate::Finding> {
        lint_sql(sql, &LintOptions::default()).unwrap()
    }

    #[test]
    fn flags_set_access_method() {
        assert!(findings("ALTER TABLE t SET ACCESS METHOD heap")
            .iter()
            .any(|f| f.rule_id == "set-access-method"));
    }

    #[test]
    fn set_access_method_is_an_error() {
        let f = findings("ALTER TABLE t SET ACCESS METHOD heap")
            .into_iter()
            .find(|f| f.rule_id == "set-access-method")
            .expect("rule must fire");
        assert_eq!(f.severity, Severity::Error);
    }

    #[test]
    fn silent_on_unrelated_alter() {
        assert!(findings("ALTER TABLE t ADD COLUMN c int")
            .iter()
            .all(|f| f.rule_id != "set-access-method"));
    }

    #[test]
    fn silent_on_same_migration_new_table() {
        // t is created empty in the same migration → rewriting it is safe → exempt.
        let sql = "CREATE TABLE t (id int); ALTER TABLE t SET ACCESS METHOD heap;";
        assert!(findings(sql)
            .iter()
            .all(|f| f.rule_id != "set-access-method"));
    }
}
