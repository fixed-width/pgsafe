//! Policy lint (opt-in, off by default): flag a foreign-key column that a `CREATE TABLE` leaves
//! nullable. A nullable FK permits orphan rows and surprising join results. NOT-NULL satisfaction
//! reuses `require_not_null` (inline NOT NULL, PK, identity, serial, or a later SET NOT NULL).
//! Engine-synthesized; not a registered `Rule`. CREATE TABLE only.

use std::collections::BTreeSet;

use pg_query::protobuf::{ConstrType, RawStmt};
use pg_query::NodeEnum;

use super::newtable::{lintable_create_relation, rangevar_key};
use super::require_not_null::{column_satisfied, columns_set_not_null, pk_column_names};
use crate::rules::{column_has_constraint, defined_columns, defined_table_constraints};

pub(crate) const ID: &str = "forbid-nullable-fk";
pub(crate) const GUIDANCE: &str =
    "Add NOT NULL to the foreign-key column (inline or a later SET NOT NULL), or suppress if a \
     nullable foreign key is intended.";

/// The foreign-key column names declared in this `CREATE TABLE` — inline `… REFERENCES` columns and
/// the local columns of each table-level `FOREIGN KEY (…)`.
fn fk_column_names(node: &NodeEnum) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for col in defined_columns(node) {
        if column_has_constraint(col, ConstrType::ConstrForeign) {
            names.insert(col.colname.clone());
        }
    }
    for con in defined_table_constraints(node) {
        if ConstrType::try_from(con.contype) == Ok(ConstrType::ConstrForeign) {
            for a in &con.fk_attrs {
                if let Some(NodeEnum::String(s)) = a.node.as_ref() {
                    names.insert(s.sval.clone());
                }
            }
        }
    }
    names
}

/// `(statement_index, message)` for each nullable foreign-key column in a `CREATE TABLE`.
pub(crate) fn nullable_fk_columns(stmts: &[RawStmt]) -> Vec<(usize, String)> {
    let set_not_null = columns_set_not_null(stmts);
    let mut out = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        let Some(rv) = lintable_create_relation(node) else {
            continue;
        };
        let fk_names = fk_column_names(node);
        if fk_names.is_empty() {
            continue;
        }
        let table = rangevar_key(rv);
        let pk_names = pk_column_names(node);
        for col in defined_columns(node) {
            if fk_names.contains(&col.colname)
                && !column_satisfied(col, &table, &pk_names, &set_not_null)
            {
                out.push((
                    i,
                    format!(
                        "The foreign-key column `{}` is nullable; a nullable foreign key allows \
                         orphan rows and unexpected join results.",
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
    use super::nullable_fk_columns;
    use crate::{lint_sql, LintOptions};

    fn enabled() -> LintOptions {
        LintOptions {
            enabled_rules: ["forbid-nullable-fk".to_string()].into_iter().collect(),
            ..LintOptions::default()
        }
    }

    fn flagged(sql: &str) -> Vec<String> {
        nullable_fk_columns(&pg_query::parse(sql).unwrap().protobuf.stmts)
            .into_iter()
            .map(|(_, m)| m)
            .collect()
    }

    #[test]
    fn nullable_inline_fk_is_flagged() {
        assert_eq!(
            flagged("CREATE TABLE t (id int, owner_id int REFERENCES users(id))").len(),
            1
        );
    }

    #[test]
    fn not_null_inline_fk_is_not_flagged() {
        assert!(
            flagged("CREATE TABLE t (id int, owner_id int NOT NULL REFERENCES users(id))")
                .is_empty()
        );
    }

    #[test]
    fn pk_fk_is_not_flagged() {
        assert!(
            flagged("CREATE TABLE t (owner_id int PRIMARY KEY REFERENCES users(id))").is_empty()
        );
    }

    #[test]
    fn table_level_fk_flags_nullable_members() {
        // composite FK on (a, b), both nullable -> two findings.
        let sql = "CREATE TABLE t (a int, b int, FOREIGN KEY (a, b) REFERENCES o(x, y))";
        assert_eq!(flagged(sql).len(), 2);
    }

    #[test]
    fn table_level_fk_flags_only_the_nullable_member() {
        // a is NOT NULL, b is nullable -> only b is flagged.
        let sql = "CREATE TABLE t (a int NOT NULL, b int, FOREIGN KEY (a, b) REFERENCES o(x, y))";
        let f = flagged(sql);
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("`b`"));
    }

    #[test]
    fn serial_fk_is_not_flagged() {
        // a serial FK column is implicitly NOT NULL.
        assert!(flagged("CREATE TABLE t (owner_id serial REFERENCES users(id))").is_empty());
    }

    #[test]
    fn identity_fk_is_not_flagged() {
        assert!(flagged(
            "CREATE TABLE t (owner_id bigint GENERATED ALWAYS AS IDENTITY REFERENCES users(id))"
        )
        .is_empty());
    }

    #[test]
    fn non_fk_nullable_column_is_not_flagged() {
        assert!(flagged("CREATE TABLE t (id int, name text)").is_empty());
    }

    #[test]
    fn fk_set_not_null_later_is_not_flagged() {
        let sql = "CREATE TABLE t (owner_id int REFERENCES users(id));\n\
                   ALTER TABLE t ALTER COLUMN owner_id SET NOT NULL;";
        assert!(flagged(sql).is_empty());
    }

    #[test]
    fn off_by_default() {
        let f = lint_sql(
            "CREATE TABLE t (owner_id int REFERENCES users(id))",
            &LintOptions::default(),
        )
        .unwrap();
        assert!(f.iter().all(|f| f.rule_id != "forbid-nullable-fk"));
    }

    #[test]
    fn fires_when_enabled() {
        use crate::Severity;
        let f = lint_sql(
            "CREATE TABLE t (owner_id int REFERENCES users(id))",
            &enabled(),
        )
        .unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "forbid-nullable-fk")
            .expect("rule must fire when enabled");
        assert_eq!(hit.severity, Severity::Warning);
    }

    #[test]
    fn suppressible_when_enabled() {
        let sql = "-- pgsafe:ignore forbid-nullable-fk intentional\n\
                   CREATE TABLE t (owner_id int REFERENCES users(id))";
        let f = lint_sql(sql, &enabled()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "forbid-nullable-fk")
            .expect("rule must fire");
        assert!(hit.is_suppressed());
    }
}
