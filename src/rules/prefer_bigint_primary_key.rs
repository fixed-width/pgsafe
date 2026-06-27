use std::collections::BTreeSet;

use pg_query::protobuf::ConstrType;
use pg_query::NodeEnum;

use super::Rule;
use crate::RuleHit;

pub struct PreferBigintPrimaryKey;

/// Integer types too small for a primary key (int4 overflows at ~2.1B rows, int2 at ~32k).
/// Covers SQL aliases and the canonical `intN` spellings; excludes int8/bigint/serial8/bigserial.
const SMALL_INT_TYPES: &[&str] = &[
    "int2",
    "smallint",
    "int4",
    "int",
    "integer",
    "serial",
    "serial4",
    "serial2",
    "smallserial",
];

impl Rule for PreferBigintPrimaryKey {
    fn id(&self) -> &'static str {
        "prefer-bigint-primary-key"
    }
    // severity() defaults to Warning.
    fn check(&self, node: &NodeEnum, out: &mut Vec<RuleHit>) {
        let cols = super::defined_columns(node);
        if cols.is_empty() {
            return;
        }

        // Collect the names of columns that are part of a PRIMARY KEY.
        let mut pk_names: BTreeSet<String> = BTreeSet::new();
        for col in &cols {
            if super::column_has_constraint(col, ConstrType::ConstrPrimary) {
                pk_names.insert(col.colname.clone()); // column-level `... PRIMARY KEY`
            }
        }
        if let NodeEnum::CreateStmt(c) = node {
            // table-level `PRIMARY KEY (a, b)` constraint among the table elements
            for elt in &c.table_elts {
                if let Some(NodeEnum::Constraint(con)) = elt.node.as_ref() {
                    if ConstrType::try_from(con.contype) == Ok(ConstrType::ConstrPrimary) {
                        for key in &con.keys {
                            if let Some(NodeEnum::String(s)) = key.node.as_ref() {
                                pk_names.insert(s.sval.clone());
                            }
                        }
                    }
                }
            }
        }

        for col in &cols {
            if pk_names.contains(&col.colname) {
                if let Some(ty) = super::column_base_type(col) {
                    if SMALL_INT_TYPES.contains(&ty.as_str()) {
                        out.push(RuleHit {
                            message: "An int4 PRIMARY KEY overflows at ~2.1 billion rows (int2 at \
                                      ~32 thousand) — a hard outage once ids run out, with no online fix."
                                .into(),
                            guidance: "Use `bigint`/`bigserial`, or `GENERATED ALWAYS AS IDENTITY`. \
                                       Migrating a live int primary key to bigint later is a major, \
                                       painful operation — start with bigint."
                                .into(),
                        });
                    }
                }
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
            .any(|f| f.rule_id == "prefer-bigint-primary-key")
    }

    #[test]
    fn flags_column_level_serial_pk() {
        assert!(fires("CREATE TABLE t (id serial PRIMARY KEY)"));
    }
    #[test]
    fn flags_column_level_int_pk() {
        assert!(fires("CREATE TABLE t (id integer PRIMARY KEY)"));
    }
    #[test]
    fn flags_table_level_pk_on_int_column() {
        assert!(fires(
            "CREATE TABLE t (id int, name text, PRIMARY KEY (id))"
        ));
    }
    #[test]
    fn flags_add_column_serial_pk() {
        assert!(fires("ALTER TABLE t ADD COLUMN id serial PRIMARY KEY"));
    }
    #[test]
    fn ignores_bigint_and_bigserial_pk() {
        assert!(!fires("CREATE TABLE t (id bigint PRIMARY KEY)"));
        assert!(!fires("CREATE TABLE t (id bigserial PRIMARY KEY)"));
        assert!(!fires("CREATE TABLE t (id bigint, PRIMARY KEY (id))"));
    }
    #[test]
    fn ignores_non_pk_int_column() {
        assert!(!fires("CREATE TABLE t (id bigint PRIMARY KEY, n int)"));
    }
    #[test]
    fn ignores_add_primary_key_on_existing_column() {
        // The pre-existing column's type isn't in the SQL — documented limitation.
        assert!(!fires("ALTER TABLE t ADD PRIMARY KEY (id)"));
    }
}
