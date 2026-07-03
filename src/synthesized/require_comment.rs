//! Policy lint (opt-in, off by default): flag a new table or column that the migration leaves without
//! a COMMENT. Cross-statement — a `COMMENT ON TABLE`/`COMMENT ON COLUMN` anywhere in the migration
//! satisfies it. Engine-synthesized; not a registered `Rule`.

use std::collections::BTreeSet;

use pg_query::protobuf::{ObjectType, RawStmt};
use pg_query::NodeEnum;

use super::newtable::{lintable_create_relation, qualified_key, rangevar_key};
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

/// The cross-statement key for a commented table (`is_column == false`) or column object path,
/// normalized the same way as the `CREATE TABLE` side (`rangevar_key` for the table, `table.column`
/// for the column, with the default `public` schema treated as bare). `None` for an unexpected shape.
fn comment_key(path: &[String], is_column: bool) -> Option<String> {
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    match (is_column, parts.as_slice()) {
        (false, [relname]) => Some(qualified_key("", relname)),
        (false, [schema, relname]) => Some(qualified_key(schema, relname)),
        (true, [table, col]) => Some(format!("{}.{col}", qualified_key("", table))),
        (true, [schema, table, col]) => Some(format!("{}.{col}", qualified_key(schema, table))),
        _ => None,
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
            match ObjectType::try_from(cs.objtype) {
                Ok(ObjectType::ObjectTable) => {
                    if let Some(key) = comment_key(&path, false) {
                        commented_tables.insert(key);
                    }
                }
                Ok(ObjectType::ObjectColumn) => {
                    if let Some(key) = comment_key(&path, true) {
                        commented_columns.insert(key);
                    }
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
        // The table whose new columns this statement introduces. A `CREATE TABLE` also requires the
        // table itself to be commented; an `ALTER TABLE … ADD COLUMN` only adds columns (the table
        // was created elsewhere), so its columns are checked but not the table.
        let (table, check_table) = match node {
            NodeEnum::CreateStmt(_) => {
                let Some(rv) = lintable_create_relation(node) else {
                    continue; // partition child or temp table — nothing to require here
                };
                (rangevar_key(rv), true)
            }
            NodeEnum::AlterTableStmt(a) => {
                // An ALTER's RangeVar carries no persistence flag, so a temp table's ADD COLUMN
                // cannot be distinguished here (rare; suppress if needed).
                let Some(rv) = a.relation.as_ref() else {
                    continue;
                };
                (rangevar_key(rv), false)
            }
            _ => continue,
        };
        if check_table && !commented_tables.contains(&table) {
            out.push((i, format!("The table `{table}` has no COMMENT.")));
        }
        for col in defined_columns(node) {
            let key = format!("{table}.{}", col.colname);
            if !commented_columns.contains(&key) {
                out.push((i, format!("The column `{key}` has no COMMENT.")));
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
    fn public_qualified_comment_satisfies_bare_table() {
        // `public.t` ≡ `t`: a public-qualified COMMENT satisfies a bare-created table and column.
        let sql = "CREATE TABLE t (id int);\n\
                   COMMENT ON TABLE public.t IS 'x';\n\
                   COMMENT ON COLUMN public.t.id IS 'y';";
        assert!(messages(sql).is_empty());
    }

    #[test]
    fn bare_comment_satisfies_public_table() {
        let sql = "CREATE TABLE public.t (id int);\n\
                   COMMENT ON TABLE t IS 'x';\n\
                   COMMENT ON COLUMN t.id IS 'y';";
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

    #[test]
    fn alter_add_column_without_comment_is_flagged() {
        // a column added by ALTER TABLE ... ADD COLUMN needs a COMMENT too (the table is not flagged).
        let m = messages("ALTER TABLE t ADD COLUMN secret text");
        assert_eq!(m.len(), 1);
        assert!(m[0].contains("`t.secret`"));
    }

    #[test]
    fn alter_add_column_with_later_comment_is_satisfied() {
        let sql = "ALTER TABLE t ADD COLUMN secret text;\n\
                   COMMENT ON COLUMN t.secret IS 'the secret';";
        assert!(messages(sql).is_empty());
    }

    #[test]
    fn alter_add_multiple_columns_yields_a_finding_each() {
        assert_eq!(
            messages("ALTER TABLE t ADD COLUMN a int, ADD COLUMN b text").len(),
            2
        );
    }

    #[test]
    fn alter_added_column_needs_comment_even_when_table_is_documented() {
        let sql = "CREATE TABLE t (id int);\n\
                   COMMENT ON TABLE t IS 'the table';\n\
                   COMMENT ON COLUMN t.id IS 'pk';\n\
                   ALTER TABLE t ADD COLUMN secret text;";
        let m = messages(sql);
        assert_eq!(m.len(), 1);
        assert!(m[0].contains("`t.secret`"));
    }

    #[test]
    fn suppressible_when_enabled() {
        // all findings for a statement share its index, so one inline ignore suppresses them together.
        let sql = "-- pgsafe:ignore require-comment lookup table\nCREATE TABLE t (id int)";
        let f = lint_sql(sql, &enabled()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "require-comment")
            .expect("rule must fire");
        assert!(hit.is_suppressed());
    }
}
