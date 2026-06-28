//! Policy lint (configured, off until set): flag a `CREATE TABLE` that the migration leaves without a
//! configured required column. Cross-statement — a column added by a later `ALTER TABLE … ADD COLUMN`
//! in the same migration counts. Engine-synthesized; not a registered `Rule`.

use std::collections::{BTreeMap, BTreeSet};

use pg_query::protobuf::RawStmt;
use pg_query::NodeEnum;

use crate::newtable::{lintable_create_relation, rangevar_key};
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
        let Some(rv) = lintable_create_relation(node) else {
            continue;
        };
        let table = rangevar_key(rv);
        let present = columns.get(&table);
        for req in required {
            // Match case-insensitively against PostgreSQL's identifier folding: an unquoted column is
            // stored lower case in the AST, so fold the required name too. An empty required name is
            // skipped (it can never name a column). A quoted, mixed-case column keeps its case and is
            // intentionally not matched. The original `req` is shown in the message.
            if req.is_empty() {
                continue;
            }
            let needle = req.to_ascii_lowercase();
            if !present.is_some_and(|s| s.contains(&needle)) {
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
    fn public_and_bare_correlate() {
        // `public.t` ≡ `t`: a bare ADD COLUMN satisfies a required column on a public-qualified
        // CREATE, and the reverse. (Auto-fixed via rangevar_key's public normalization.)
        assert!(flagged(
            "CREATE TABLE public.t (id int); ALTER TABLE t ADD COLUMN created_at timestamptz;",
            &["created_at"]
        )
        .is_empty());
        assert!(flagged(
            "CREATE TABLE t (id int); ALTER TABLE public.t ADD COLUMN created_at timestamptz;",
            &["created_at"]
        )
        .is_empty());
    }

    #[test]
    fn non_public_schema_alter_does_not_satisfy() {
        // app.t is a different table than bare t, so its ADD COLUMN does not satisfy t's requirement.
        assert_eq!(
            flagged(
                "CREATE TABLE t (id int); ALTER TABLE app.t ADD COLUMN created_at timestamptz;",
                &["created_at"]
            )
            .len(),
            1
        );
    }

    #[test]
    fn required_name_matches_case_insensitively() {
        // a mixed-case required name matches an unquoted (lower-folded) column; still flagged when
        // the column is genuinely absent.
        assert!(flagged(
            "CREATE TABLE t (id int, created_at timestamptz)",
            &["Created_At"]
        )
        .is_empty());
        assert_eq!(flagged("CREATE TABLE t (id int)", &["Created_At"]).len(), 1);
    }

    #[test]
    fn empty_required_name_is_skipped() {
        // an empty required name can never match a column, but must not flag every table.
        assert!(flagged("CREATE TABLE t (id int)", &[""]).is_empty());
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
    fn mixed_case_lint_options_name_matches_via_lint_sql() {
        // A direct library caller passing a mixed-case name in LintOptions must not get a false
        // positive: the rule folds it to match the lower-folded column.
        let f = lint_sql(
            "CREATE TABLE t (id int, created_at timestamptz)",
            &opts(&["Created_At"]),
        )
        .unwrap();
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
