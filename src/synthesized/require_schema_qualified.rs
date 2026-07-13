//! Policy lint (opt-in, off by default): flag a DDL statement whose **target** table is named
//! without a schema qualifier — it resolves through `search_path`, which is environment-dependent
//! and a migration footgun. Covers the target `RangeVar` of `CREATE TABLE`, `ALTER TABLE`,
//! `CREATE INDEX`, and `TRUNCATE`. Temp targets (resolved in `pg_temp`) are exempt.
//! Engine-synthesized, gated on being enabled in the config; not a registered `Rule`.

use crate::ast::protobuf::{RangeVar, RawStmt};
use crate::ast::NodeEnum;

pub(crate) const ID: &str = "require-schema-qualified";
pub(crate) const GUIDANCE: &str =
    "Qualify the table name with its schema (e.g. `public.<name>`) so resolution does not depend on \
    the session's search_path.";

/// The DDL **target** relations a statement operates on (empty for non-DDL / unsupported nodes).
fn target_rangevars(node: &NodeEnum) -> Vec<&RangeVar> {
    match node {
        NodeEnum::CreateStmt(c) => c.relation.as_ref().into_iter().collect(),
        NodeEnum::AlterTableStmt(a) => a.relation.as_ref().into_iter().collect(),
        NodeEnum::IndexStmt(i) => i.relation.as_ref().into_iter().collect(),
        NodeEnum::TruncateStmt(t) => t
            .relations
            .iter()
            .filter_map(|n| match n.node.as_ref()? {
                NodeEnum::RangeVar(rv) => Some(rv),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `(statement_index, table_name)` for every DDL target whose name is unqualified (empty
/// `schemaname`). Temp targets (`relpersistence == "t"`) are exempt — they intentionally resolve
/// in `pg_temp`.
pub(crate) fn unqualified_targets(stmts: &[RawStmt]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        for rv in target_rangevars(node) {
            if rv.relpersistence == "t" {
                continue;
            }
            if rv.schemaname.is_empty() && !rv.relname.is_empty() {
                out.push((i, rv.relname.clone()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::unqualified_targets;
    use crate::{lint_sql, LintOptions};

    fn enabled() -> LintOptions {
        LintOptions {
            enabled_rules: ["require-schema-qualified".to_string()]
                .into_iter()
                .collect(),
            ..LintOptions::default()
        }
    }
    fn flagged(sql: &str) -> Vec<(usize, String)> {
        unqualified_targets(&crate::ast::parse(sql).unwrap().protobuf.stmts)
    }

    #[test]
    fn flags_unqualified_create_and_alter() {
        assert_eq!(
            flagged("CREATE TABLE t (id int)"),
            vec![(0, "t".to_string())]
        );
        assert_eq!(
            flagged("ALTER TABLE orders ADD COLUMN x int"),
            vec![(0, "orders".to_string())]
        );
    }

    #[test]
    fn ignores_schema_qualified() {
        assert!(flagged("CREATE TABLE public.t (id int)").is_empty());
        assert!(flagged("ALTER TABLE app.orders ADD COLUMN x int").is_empty());
    }

    #[test]
    fn ignores_temp_table() {
        assert!(flagged("CREATE TEMP TABLE t (id int)").is_empty());
    }

    #[test]
    fn flags_unqualified_index_and_truncate() {
        assert_eq!(
            flagged("CREATE INDEX i ON t (x)"),
            vec![(0, "t".to_string())]
        );
        assert_eq!(flagged("TRUNCATE t"), vec![(0, "t".to_string())]);
    }

    #[test]
    fn off_by_default() {
        assert!(lint_sql("CREATE TABLE t (id int)", &LintOptions::default())
            .unwrap()
            .iter()
            .all(|f| f.rule_id != "require-schema-qualified"));
    }

    #[test]
    fn fires_when_enabled_with_table_name_in_message() {
        use crate::Severity;
        let f = lint_sql("CREATE TABLE t (id int)", &enabled()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "require-schema-qualified")
            .expect("must fire when enabled");
        assert_eq!(hit.severity, Severity::Warning);
        assert!(hit.message.contains('`'), "message names the table");
    }

    #[test]
    fn inline_suppressible_when_enabled() {
        let sql = "-- pgsafe:ignore require-schema-qualified intentional search_path use\n\
                   CREATE TABLE t (id int)";
        let hit = lint_sql(sql, &enabled())
            .unwrap()
            .into_iter()
            .find(|f| f.rule_id == "require-schema-qualified")
            .expect("rule must fire");
        assert!(hit.is_suppressed());
    }
}
