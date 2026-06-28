//! Policy lint (configured, off until set): flag a `CREATE TABLE` that the migration leaves without a
//! configured required column. Cross-statement — a column added by a later `ALTER TABLE … ADD COLUMN`
//! in the same migration counts. Engine-synthesized; not a registered `Rule`.

use std::collections::{BTreeMap, BTreeSet};

use pg_query::protobuf::RawStmt;
use pg_query::NodeEnum;

use crate::newtable::rangevar_key;
use crate::rules::defined_columns;

pub(crate) const ID: &str = "require-columns";
pub(crate) const GUIDANCE: &str =
    "Add the column to the CREATE TABLE (or a later ALTER TABLE … ADD COLUMN in the same migration), \
     or remove it from required-columns in your config.";

/// `(statement_index, message)` for each required column a `CREATE TABLE` is missing.
pub(crate) fn missing_required_columns(
    stmts: &[RawStmt],
    required: &BTreeSet<String>,
) -> Vec<(usize, String)> {
    if required.is_empty() {
        return Vec::new();
    }
    // table key -> every column name introduced for it (CREATE columns + later ADD COLUMN).
    let mut columns: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for raw in stmts {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        let table = match node {
            NodeEnum::CreateStmt(c) => c.relation.as_ref().map(rangevar_key),
            NodeEnum::AlterTableStmt(a) => a.relation.as_ref().map(rangevar_key),
            _ => None,
        };
        let Some(table) = table else {
            continue;
        };
        let entry = columns.entry(table).or_default();
        for col in defined_columns(node) {
            entry.insert(col.colname.clone());
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
        let present = columns.get(&table);
        for req in required {
            if !present.is_some_and(|s| s.contains(req)) {
                out.push((
                    i,
                    format!("The table `{table}` is missing the required column `{req}`."),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::missing_required_columns;
    use crate::{lint_sql, LintOptions};

    fn req(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    fn flagged(sql: &str, names: &[&str]) -> Vec<String> {
        missing_required_columns(&pg_query::parse(sql).unwrap().protobuf.stmts, &req(names))
            .into_iter()
            .map(|(_, m)| m)
            .collect()
    }

    #[test]
    fn missing_required_column_is_flagged() {
        assert_eq!(flagged("CREATE TABLE t (id int)", &["created_at"]).len(), 1);
    }

    #[test]
    fn present_inline_column_is_not_flagged() {
        assert!(flagged(
            "CREATE TABLE t (id int, created_at timestamptz)",
            &["created_at"]
        )
        .is_empty());
    }

    #[test]
    fn column_added_by_later_alter_satisfies() {
        let sql = "CREATE TABLE t (id int);\nALTER TABLE t ADD COLUMN created_at timestamptz;";
        assert!(flagged(sql, &["created_at"]).is_empty());
    }

    #[test]
    fn two_missing_columns_yield_two_findings() {
        assert_eq!(
            flagged("CREATE TABLE t (id int)", &["created_at", "updated_at"]).len(),
            2
        );
    }

    #[test]
    fn temp_table_is_not_flagged() {
        assert!(flagged("CREATE TEMP TABLE t (id int)", &["created_at"]).is_empty());
    }

    #[test]
    fn empty_config_does_no_work() {
        assert!(flagged("CREATE TABLE t (id int)", &[]).is_empty());
    }

    fn opts(names: &[&str]) -> LintOptions {
        LintOptions {
            required_columns: req(names),
            ..LintOptions::default()
        }
    }

    #[test]
    fn off_without_config() {
        let f = lint_sql("CREATE TABLE t (id int)", &LintOptions::default()).unwrap();
        assert!(f.iter().all(|f| f.rule_id != "require-columns"));
    }

    #[test]
    fn fires_with_config() {
        use crate::Severity;
        let f = lint_sql("CREATE TABLE t (id int)", &opts(&["created_at"])).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "require-columns")
            .expect("rule must fire when configured");
        assert_eq!(hit.severity, Severity::Warning);
    }

    #[test]
    fn suppressible() {
        let sql = "-- pgsafe:ignore require-columns lookup table\nCREATE TABLE t (id int)";
        let f = lint_sql(sql, &opts(&["created_at"])).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "require-columns")
            .expect("rule must fire");
        assert!(hit.is_suppressed());
    }
}
