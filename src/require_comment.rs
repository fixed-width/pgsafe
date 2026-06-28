//! Policy lint (opt-in, off by default): flag a new table or column that the migration leaves without
//! a COMMENT. Cross-statement — a `COMMENT ON TABLE`/`COMMENT ON COLUMN` anywhere in the migration
//! satisfies it. Engine-synthesized; not a registered `Rule`.

use std::collections::BTreeSet;

use pg_query::protobuf::{ObjectType, RawStmt};
use pg_query::NodeEnum;

use crate::newtable::rangevar_key;
use crate::rules::defined_columns;

pub(crate) const ID: &str = "require-comment";
pub(crate) const GUIDANCE: &str =
    "Add a COMMENT ON TABLE / COMMENT ON COLUMN in the migration documenting the new object.";

/// The dotted name parts of a `CommentStmt` object (`["t"]`, `["s", "t"]`, or `["t", "c"]`).
fn object_path(object: Option<&NodeEnum>) -> Vec<String> {
    match object {
        Some(NodeEnum::List(l)) => l
            .items
            .iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(NodeEnum::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .collect(),
        Some(NodeEnum::String(s)) => vec![s.sval.clone()],
        _ => Vec::new(),
    }
}

/// `(statement_index, message)` for each new table/column the migration leaves without a COMMENT.
pub(crate) fn missing_comments(stmts: &[RawStmt]) -> Vec<(usize, String)> {
    let mut commented_tables: BTreeSet<String> = BTreeSet::new();
    let mut commented_columns: BTreeSet<String> = BTreeSet::new();
    for raw in stmts {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        if let NodeEnum::CommentStmt(cs) = node {
            let path = object_path(cs.object.as_ref().and_then(|b| b.node.as_ref()));
            if path.is_empty() {
                continue;
            }
            let key = path.join(".");
            match ObjectType::try_from(cs.objtype) {
                Ok(ObjectType::ObjectTable) => {
                    commented_tables.insert(key);
                }
                Ok(ObjectType::ObjectColumn) => {
                    commented_columns.insert(key);
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        let NodeEnum::CreateStmt(c) = node else {
            continue;
        };
        if c.partbound.is_some() {
            continue;
        }
        let Some(rv) = c.relation.as_ref() else {
            continue;
        };
        if rv.relpersistence == "t" {
            continue;
        }
        let table = rangevar_key(rv);
        if !commented_tables.contains(&table) {
            out.push((i, format!("The table `{table}` has no COMMENT.")));
        }
        for col in defined_columns(node) {
            let key = format!("{table}.{}", col.colname);
            if !commented_columns.contains(&key) {
                out.push((i, format!("The column `{}` has no COMMENT.", key)));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::missing_comments;
    use crate::{lint_sql, LintOptions};

    fn enabled() -> LintOptions {
        LintOptions {
            enabled_rules: ["require-comment".to_string()].into_iter().collect(),
            ..LintOptions::default()
        }
    }

    fn messages(sql: &str) -> Vec<String> {
        missing_comments(&pg_query::parse(sql).unwrap().protobuf.stmts)
            .into_iter()
            .map(|(_, m)| m)
            .collect()
    }

    #[test]
    fn uncommented_table_and_columns_are_flagged() {
        // table + one column = two findings.
        assert_eq!(messages("CREATE TABLE t (id int)").len(), 2);
    }

    #[test]
    fn fully_commented_table_is_silent() {
        let sql = "CREATE TABLE t (id int);\n\
                   COMMENT ON TABLE t IS 'the t table';\n\
                   COMMENT ON COLUMN t.id IS 'identifier';";
        assert!(messages(sql).is_empty());
    }

    #[test]
    fn missing_column_comment_is_flagged_when_table_is_commented() {
        let sql = "CREATE TABLE t (id int);\nCOMMENT ON TABLE t IS 'x';";
        let m = messages(sql);
        assert_eq!(m.len(), 1);
        assert!(m[0].contains("`t.id`"));
    }

    #[test]
    fn temp_table_is_not_flagged() {
        assert!(messages("CREATE TEMP TABLE t (id int)").is_empty());
    }

    #[test]
    fn off_by_default() {
        let f = lint_sql("CREATE TABLE t (id int)", &LintOptions::default()).unwrap();
        assert!(f.iter().all(|f| f.rule_id != "require-comment"));
    }

    #[test]
    fn fires_when_enabled() {
        use crate::Severity;
        let f = lint_sql("CREATE TABLE t (id int)", &enabled()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "require-comment")
            .expect("rule must fire when enabled");
        assert_eq!(hit.severity, Severity::Warning);
    }
}
