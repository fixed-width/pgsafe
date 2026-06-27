use pg_query::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct PreferJsonb;

impl Rule for PreferJsonb {
    fn id(&self) -> &'static str {
        "prefer-jsonb"
    }
    // severity() defaults to Warning.
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        for col in super::defined_columns(node) {
            if super::column_base_type(col).as_deref() == Some("json") {
                out.push(RuleHit {
                    message: "A `json` column has no equality or ordering operators, so \
                              SELECT DISTINCT, GROUP BY, UNION, and ORDER BY on it fail at query time."
                        .into(),
                    guidance: "Use `jsonb` instead — it supports those operators and indexing. `json` \
                               only preserves exact input text and duplicate/key order, which is rarely \
                               needed."
                        .into(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lint_sql, LintOptions};

    fn fires(sql: &str) -> bool {
        lint_sql(sql, &LintOptions::default())
            .unwrap()
            .iter()
            .any(|f| f.rule_id == "prefer-jsonb")
    }

    #[test]
    fn flags_json_in_create_table() {
        assert!(fires("CREATE TABLE t (id int, data json)"));
    }
    #[test]
    fn flags_json_in_add_column() {
        assert!(fires("ALTER TABLE t ADD COLUMN data json"));
    }
    #[test]
    fn ignores_jsonb() {
        assert!(!fires("CREATE TABLE t (id int, data jsonb)"));
        assert!(!fires("ALTER TABLE t ADD COLUMN data jsonb"));
    }
}
