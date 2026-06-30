use std::collections::BTreeSet;

use pg_query::protobuf::{ColumnDef, ConstrType};
use pg_query::NodeEnum;

use super::Rule;
use crate::fix::{FixAnchor, FixDraft, FixDraftEdit};
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

/// Build a fix draft that upgrades a small-integer type to `bigint` or `bigserial`.
///
/// Serial aliases (`serial`, `serial2`, `serial4`, `smallserial`) carry implicit
/// auto-increment sequence semantics; upgrading them to plain `bigint` would silently
/// drop that, so they become `bigserial` instead.
///
/// Guards: `type_name` must be `Some` and `location >= 0` (pg_query sets -1 when the
/// source position is unknown).
fn bigint_fix(ty: &str, col: &ColumnDef) -> Option<FixDraft> {
    let replacement = if matches!(ty, "serial" | "serial2" | "serial4" | "smallserial") {
        "bigserial"
    } else {
        "bigint"
    };
    let tn = col.type_name.as_ref()?;
    // pg_query sets location to -1 when the source position is unknown; reject those.
    let at = u32::try_from(tn.location).ok()?;
    Some(FixDraft {
        title: "Use bigint",
        edits: vec![FixDraftEdit {
            anchor: FixAnchor::ReplaceTokenAt(at),
            replacement: replacement.into(),
        }],
    })
}

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
        // Table-level `PRIMARY KEY (a, b)` — among CREATE TABLE elements, or an ALTER TABLE
        // ADD CONSTRAINT command (so `ADD COLUMN id int, ADD PRIMARY KEY (id)` in one statement,
        // where the column's type IS visible, is also caught).
        let table_level_constraints = match node {
            NodeEnum::CreateStmt(c) => c
                .table_elts
                .iter()
                .filter_map(|elt| match elt.node.as_ref()? {
                    NodeEnum::Constraint(con) => Some(con.as_ref()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            NodeEnum::AlterTableStmt(_) => super::constraints_being_added(node),
            _ => Vec::new(),
        };
        for con in table_level_constraints {
            if ConstrType::try_from(con.contype) == Ok(ConstrType::ConstrPrimary) {
                for key in &con.keys {
                    if let Some(NodeEnum::String(s)) = key.node.as_ref() {
                        pk_names.insert(s.sval.clone());
                    }
                }
            }
        }

        for col in &cols {
            if pk_names.contains(&col.colname) {
                if let Some(ty) = super::column_base_type(col) {
                    if SMALL_INT_TYPES.contains(&ty.as_str()) {
                        out.push(RuleHit {
                            message: "A small-integer PRIMARY KEY overflows its id space (int4 at \
                                      ~2.1 billion rows, int2 at ~32 thousand) — a hard outage once \
                                      ids run out, with no online fix."
                                .into(),
                            guidance: "Use `bigint`/`bigserial`, or `GENERATED ALWAYS AS IDENTITY`. \
                                       Migrating a live int primary key to bigint later is a major, \
                                       painful operation — start with bigint."
                                .into(),
                            fix: bigint_fix(&ty, col),
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
    fn flags_compound_add_column_with_table_level_pk() {
        // The column is added and made a PK in the same statement — its type IS visible.
        assert!(fires(
            "ALTER TABLE t ADD COLUMN id int, ADD PRIMARY KEY (id)"
        ));
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

    #[test]
    fn emits_bigint_fix_on_integer_pk_and_clears() {
        use crate::fix::apply;
        let sql = "CREATE TABLE t (id integer PRIMARY KEY)";
        let fs = lint_sql(sql, &LintOptions::default()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "prefer-bigint-primary-key")
            .unwrap();
        let fix = f.fix.as_ref().expect("fix present");
        assert_eq!(fix.title, "Use bigint");
        let fixed = apply(sql, fix);
        assert_eq!(fixed, "CREATE TABLE t (id bigint PRIMARY KEY)");
        assert!(
            lint_sql(&fixed, &LintOptions::default())
                .unwrap()
                .iter()
                .all(|f| f.rule_id != "prefer-bigint-primary-key"),
            "fixed SQL must not re-trigger prefer-bigint-primary-key"
        );
    }

    #[test]
    fn emits_bigserial_fix_on_serial_pk_and_clears() {
        use crate::fix::apply;
        let sql = "CREATE TABLE t (id serial PRIMARY KEY)";
        let fs = lint_sql(sql, &LintOptions::default()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "prefer-bigint-primary-key")
            .unwrap();
        let fix = f.fix.as_ref().expect("fix present");
        let fixed = apply(sql, fix);
        // serial → bigserial preserves the auto-increment sequence; plain bigint would not.
        assert_eq!(fixed, "CREATE TABLE t (id bigserial PRIMARY KEY)");
        assert!(
            lint_sql(&fixed, &LintOptions::default())
                .unwrap()
                .iter()
                .all(|f| f.rule_id != "prefer-bigint-primary-key"),
            "fixed SQL must not re-trigger prefer-bigint-primary-key"
        );
    }

    #[test]
    fn emits_bigint_fix_on_table_level_pk_and_clears() {
        use crate::fix::apply;
        let sql = "CREATE TABLE t (id int, PRIMARY KEY (id))";
        let fs = lint_sql(sql, &LintOptions::default()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "prefer-bigint-primary-key")
            .unwrap();
        let fix = f.fix.as_ref().expect("fix present");
        let fixed = apply(sql, fix);
        assert_eq!(fixed, "CREATE TABLE t (id bigint, PRIMARY KEY (id))");
        assert!(
            lint_sql(&fixed, &LintOptions::default())
                .unwrap()
                .iter()
                .all(|f| f.rule_id != "prefer-bigint-primary-key"),
            "fixed SQL must not re-trigger prefer-bigint-primary-key"
        );
    }
}
