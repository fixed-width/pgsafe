//! Policy lint (opt-in, off by default): flag a column a `CREATE TABLE` leaves nullable. A column is
//! satisfied by an inline `NOT NULL`, a primary key (inline or table-level), an identity column, a
//! serial pseudo-type, or a later `ALTER TABLE … ALTER COLUMN … SET NOT NULL` in the same migration
//! (cross-statement final-state). Engine-synthesized, gated on being enabled; not a registered `Rule`.

use std::collections::BTreeSet;

use crate::ast::protobuf::{AlterTableType, ColumnDef, ConstrType, RawStmt};
use crate::ast::NodeEnum;

use super::newtable::{lintable_create_relation, rangevar_key};
use crate::rules::{
    alter_table_cmds, column_base_type, column_has_constraint, defined_columns,
    defined_table_constraints,
};

pub(crate) const ID: &str = "require-not-null";
pub(crate) const GUIDANCE: &str =
    "Add NOT NULL to the column (it is free on a new, empty table), or add it later in the same \
     migration with ALTER TABLE ... ALTER COLUMN ... SET NOT NULL. For an intentionally nullable \
     column, suppress with `-- pgsafe:ignore require-not-null ...`.";

/// Serial pseudo-types — each is sugar for a NOT NULL integer column.
const SERIAL_TYPES: &[&str] = &[
    "serial",
    "serial2",
    "serial4",
    "serial8",
    "smallserial",
    "bigserial",
];

/// `(table_key, column_name)` pairs made non-null by an `ALTER TABLE … ALTER COLUMN … SET NOT NULL`
/// anywhere in the migration.
pub(crate) fn columns_set_not_null(stmts: &[RawStmt]) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for raw in stmts {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        let NodeEnum::AlterTableStmt(a) = node else {
            continue;
        };
        let Some(rv) = a.relation.as_ref() else {
            continue;
        };
        let table = rangevar_key(rv);
        for cmd in alter_table_cmds(node) {
            if AlterTableType::try_from(cmd.subtype) == Ok(AlterTableType::AtSetNotNull) {
                out.insert((table.clone(), cmd.name.clone()));
            }
        }
    }
    out
}

/// The names of columns covered by a primary key declared in this statement — an inline column
/// `PRIMARY KEY` and a table-level `PRIMARY KEY (…)` (a composite PK covers each key column).
pub(crate) fn pk_column_names(node: &NodeEnum) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for col in defined_columns(node) {
        if column_has_constraint(col, ConstrType::ConstrPrimary) {
            names.insert(col.colname.clone());
        }
    }
    for con in defined_table_constraints(node) {
        if ConstrType::try_from(con.contype) == Ok(ConstrType::ConstrPrimary) {
            for key in &con.keys {
                if let Some(NodeEnum::String(s)) = key.node.as_ref() {
                    names.insert(s.sval.clone());
                }
            }
        }
    }
    names
}

/// Whether a column is guaranteed non-null by its own definition, this statement's primary key, or a
/// later `SET NOT NULL` on its table.
pub(crate) fn column_satisfied(
    col: &ColumnDef,
    table: &str,
    pk_names: &BTreeSet<String>,
    set_not_null: &BTreeSet<(String, String)>,
) -> bool {
    column_has_constraint(col, ConstrType::ConstrNotnull)
        || column_has_constraint(col, ConstrType::ConstrPrimary)
        || column_has_constraint(col, ConstrType::ConstrIdentity)
        || pk_names.contains(&col.colname)
        || column_base_type(col).is_some_and(|t| SERIAL_TYPES.contains(&t.as_str()))
        || set_not_null.contains(&(table.to_string(), col.colname.clone()))
}

/// `(statement_index, message)` for each nullable column in a `CREATE TABLE` the migration leaves
/// without a NOT NULL guarantee. Persistent, non-partition-child tables only.
pub(crate) fn nullable_columns(stmts: &[RawStmt]) -> Vec<(usize, String)> {
    let set_not_null = columns_set_not_null(stmts);
    let mut out = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        let Some(rv) = lintable_create_relation(node) else {
            continue;
        };
        let table = rangevar_key(rv);
        let pk_names = pk_column_names(node);
        for col in defined_columns(node) {
            if !column_satisfied(col, &table, &pk_names, &set_not_null) {
                out.push((
                    i,
                    format!(
                        "The column `{}` has no NOT NULL constraint; this policy requires every \
                         column in a CREATE TABLE to be NOT NULL.",
                        col.colname
                    ),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::nullable_columns;
    use crate::{lint_sql, LintOptions};

    fn enabled_opts() -> LintOptions {
        LintOptions {
            enabled_rules: ["require-not-null".to_string()].into_iter().collect(),
            ..LintOptions::default()
        }
    }

    fn flagged(sql: &str) -> Vec<String> {
        nullable_columns(&crate::ast::parse(sql).unwrap().protobuf.stmts)
            .into_iter()
            .map(|(_, m)| m)
            .collect()
    }

    #[test]
    fn nullable_column_is_flagged() {
        assert_eq!(flagged("CREATE TABLE t (email text)").len(), 1);
    }

    #[test]
    fn not_null_column_is_not_flagged() {
        assert!(flagged("CREATE TABLE t (email text NOT NULL)").is_empty());
    }

    #[test]
    fn explicit_null_column_is_flagged() {
        // An explicit NULL constraint is still nullable — the policy flags it.
        assert_eq!(flagged("CREATE TABLE t (email text NULL)").len(), 1);
    }

    #[test]
    fn inline_primary_key_is_not_flagged() {
        assert!(flagged("CREATE TABLE t (id int PRIMARY KEY)").is_empty());
    }

    #[test]
    fn table_level_pk_covers_its_columns_only() {
        // `id` is covered by the table-level PK; the nullable `name` is still flagged.
        let f = flagged("CREATE TABLE t (id int, name text, PRIMARY KEY (id))");
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("`name`"));
    }

    #[test]
    fn composite_pk_covers_all_key_columns() {
        assert!(flagged("CREATE TABLE t (a int, b int, PRIMARY KEY (a, b))").is_empty());
    }

    #[test]
    fn identity_column_is_not_flagged() {
        assert!(flagged("CREATE TABLE t (id int GENERATED ALWAYS AS IDENTITY)").is_empty());
    }

    #[test]
    fn by_default_identity_column_is_not_flagged() {
        assert!(flagged("CREATE TABLE t (id int GENERATED BY DEFAULT AS IDENTITY)").is_empty());
    }

    #[test]
    fn serial_column_is_not_flagged() {
        assert!(flagged("CREATE TABLE t (id bigserial)").is_empty());
    }

    #[test]
    fn column_set_not_null_later_is_not_flagged() {
        assert!(flagged(
            "CREATE TABLE t (email text); ALTER TABLE t ALTER COLUMN email SET NOT NULL;"
        )
        .is_empty());
    }

    #[test]
    fn temp_table_is_not_flagged() {
        assert!(flagged("CREATE TEMP TABLE t (email text)").is_empty());
    }

    #[test]
    fn partition_child_is_not_flagged() {
        assert!(flagged("CREATE TABLE c PARTITION OF p FOR VALUES FROM (0) TO (100)").is_empty());
    }

    #[test]
    fn each_nullable_column_yields_a_finding() {
        assert_eq!(flagged("CREATE TABLE t (a int, b int)").len(), 2);
    }

    #[test]
    fn off_by_default() {
        let f = lint_sql("CREATE TABLE t (email text)", &LintOptions::default()).unwrap();
        assert!(f.iter().all(|f| f.rule_id != "require-not-null"));
    }

    #[test]
    fn fires_when_enabled() {
        use crate::Severity;
        let f = lint_sql("CREATE TABLE t (email text)", &enabled_opts()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "require-not-null")
            .expect("rule must fire when enabled");
        assert_eq!(hit.severity, Severity::Warning);
    }

    #[test]
    fn passes_when_all_columns_not_null() {
        let f = lint_sql(
            "CREATE TABLE t (id int PRIMARY KEY, email text NOT NULL)",
            &enabled_opts(),
        )
        .unwrap();
        assert!(f.iter().all(|f| f.rule_id != "require-not-null"));
    }

    #[test]
    fn inline_suppressible_when_enabled() {
        let sql = "-- pgsafe:ignore require-not-null nullable by design\n\
                   CREATE TABLE t (email text)";
        let f = lint_sql(sql, &enabled_opts()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "require-not-null")
            .expect("rule must fire");
        assert!(hit.is_suppressed());
    }
}
