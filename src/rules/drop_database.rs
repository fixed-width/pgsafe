use crate::ast::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct DropDatabase;

impl Rule for DropDatabase {
    fn id(&self) -> &'static str {
        "drop-database"
    }
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        // `DropdbStmt` is produced only for `DROP DATABASE`, so no sub-filtering is needed.
        if matches!(node, NodeEnum::DropdbStmt(_)) {
            out.push(RuleHit {
                message: "DROP DATABASE permanently and irreversibly removes the database and all its \
                          contents. It fails while any session is still connected, unless run WITH (FORCE), \
                          which terminates those sessions and loses their in-flight work."
                    .into(),
                guidance: "Confirm the database is fully retired and has no active connections before \
                           dropping it; take a final backup first if the data may be needed."
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

    #[test]
    fn flags_drop_database() {
        let f = findings("DROP DATABASE olddb")
            .into_iter()
            .find(|f| f.rule_id == "drop-database")
            .expect("rule must fire");
        assert_eq!(f.severity, Severity::Warning);
    }

    #[test]
    fn flags_drop_database_if_exists() {
        assert!(findings("DROP DATABASE IF EXISTS olddb")
            .iter()
            .any(|f| f.rule_id == "drop-database"));
    }

    #[test]
    fn silent_on_drop_table() {
        assert!(findings("DROP TABLE t")
            .iter()
            .all(|f| f.rule_id != "drop-database"));
    }
}
