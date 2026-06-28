//! Cross-statement foreign-key index check: flag a foreign key on a column the
//! migration **creates or adds in the same statement** when no index built anywhere
//! in the migration covers that column (leads with it). A foreign key with no
//! covering index makes the parent's referential checks — and its `ON DELETE` /
//! `ON UPDATE` actions — scan and lock the child on every change. Scoped to new
//! columns because a static linter cannot know whether a pre-existing column is
//! already indexed (the live-schema check is a separate feature). This is an
//! engine-synthesized finding, not a registered `Rule`.

use std::collections::{BTreeMap, BTreeSet};

use pg_query::protobuf::{ConstrType, Node, RawStmt};
use pg_query::NodeEnum;

use crate::newtable::rangevar_key;
use crate::rules::{column_has_constraint, defined_columns, defined_table_constraints};

pub(crate) const ID: &str = "fk-without-covering-index";

/// The string value of a `String` AST node (column-name lists hold these), if it is one.
fn node_string(n: &Node) -> Option<&str> {
    match n.node.as_ref()? {
        NodeEnum::String(s) => Some(s.sval.as_str()),
        _ => None,
    }
}

/// The leading column of an index's `index_params`, skipping expression elements
/// (which have an empty `name`).
fn first_index_col(params: &[Node]) -> Option<&str> {
    match params.first()?.node.as_ref()? {
        NodeEnum::IndexElem(e) if !e.name.is_empty() => Some(e.name.as_str()),
        _ => None,
    }
}

/// The table a `CREATE TABLE` / `ALTER TABLE` operates on (FK columns live here).
fn table_key(node: &NodeEnum) -> Option<String> {
    let rv = match node {
        NodeEnum::CreateStmt(c) => c.relation.as_ref(),
        NodeEnum::AlterTableStmt(a) => a.relation.as_ref(),
        _ => None,
    }?;
    Some(rangevar_key(rv))
}

/// Record every leading column covered by an index this statement creates, keyed by table.
fn collect_covered(node: &NodeEnum, covered: &mut BTreeMap<String, BTreeSet<String>>) {
    match node {
        NodeEnum::IndexStmt(idx) => {
            if let (Some(rv), Some(col)) =
                (idx.relation.as_ref(), first_index_col(&idx.index_params))
            {
                covered
                    .entry(rangevar_key(rv))
                    .or_default()
                    .insert(col.to_string());
            }
        }
        NodeEnum::CreateStmt(_) | NodeEnum::AlterTableStmt(_) => {
            let Some(table) = table_key(node) else {
                return;
            };
            // Column-level PRIMARY KEY / UNIQUE implicitly builds a leading index on the column.
            for col in defined_columns(node) {
                if column_has_constraint(col, ConstrType::ConstrPrimary)
                    || column_has_constraint(col, ConstrType::ConstrUnique)
                {
                    covered
                        .entry(table.clone())
                        .or_default()
                        .insert(col.colname.clone());
                }
            }
            // Table-level PRIMARY KEY / UNIQUE — its first key is the leading column.
            for con in defined_table_constraints(node) {
                let t = ConstrType::try_from(con.contype);
                if t == Ok(ConstrType::ConstrPrimary) || t == Ok(ConstrType::ConstrUnique) {
                    if let Some(first) = con.keys.first().and_then(node_string) {
                        covered
                            .entry(table.clone())
                            .or_default()
                            .insert(first.to_string());
                    }
                }
            }
        }
        _ => {}
    }
}

/// Record every foreign key this statement declares on a NEW column, as
/// `(statement_index, table_key, referencing_column)` (multi-column FKs key on the first).
fn collect_new_fks(i: usize, node: &NodeEnum, out: &mut Vec<(usize, String, String)>) {
    let Some(table) = table_key(node) else {
        return;
    };
    let new_cols: BTreeSet<&str> = defined_columns(node)
        .iter()
        .map(|c| c.colname.as_str())
        .collect();
    // Column-level `... REFERENCES`: the column itself is new by definition.
    for col in defined_columns(node) {
        if column_has_constraint(col, ConstrType::ConstrForeign) {
            out.push((i, table.clone(), col.colname.clone()));
        }
    }
    // Table-level `FOREIGN KEY (col, ...)`: flag only when the first column is new here.
    for con in defined_table_constraints(node) {
        if ConstrType::try_from(con.contype) == Ok(ConstrType::ConstrForeign) {
            if let Some(first) = con.fk_attrs.first().and_then(node_string) {
                if new_cols.contains(first) {
                    out.push((i, table.clone(), first.to_string()));
                }
            }
        }
    }
}

/// New-column foreign keys with no covering index anywhere in the migration, as
/// `(statement_index, table_key, referencing_column)`. Duplicates (same table+column)
/// collapse to the first occurrence.
pub(crate) fn fk_without_index(stmts: &[RawStmt]) -> Vec<(usize, String, String)> {
    let mut covered: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut new_fks: Vec<(usize, String, String)> = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        collect_covered(node, &mut covered);
        collect_new_fks(i, node, &mut new_fks);
    }
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    new_fks
        .into_iter()
        .filter(|(_, table, col)| !covered.get(table).is_some_and(|set| set.contains(col)))
        .filter(|(_, table, col)| seen.insert((table.clone(), col.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::fk_without_index;

    fn flagged(sql: &str) -> Vec<(usize, String, String)> {
        fk_without_index(&pg_query::parse(sql).unwrap().protobuf.stmts)
    }

    #[test]
    fn new_column_fk_without_index_is_flagged() {
        let f = flagged("CREATE TABLE child (id bigint, parent_id bigint REFERENCES parent (id))");
        assert_eq!(f, vec![(0, "child".to_string(), "parent_id".to_string())]);
    }

    #[test]
    fn add_column_fk_without_index_is_flagged() {
        let f = flagged("ALTER TABLE child ADD COLUMN parent_id bigint REFERENCES parent");
        assert_eq!(f, vec![(0, "child".to_string(), "parent_id".to_string())]);
    }

    #[test]
    fn a_following_create_index_covers_it() {
        assert!(flagged(
            "CREATE TABLE child (parent_id bigint REFERENCES parent); \
             CREATE INDEX ON child (parent_id)"
        )
        .is_empty());
    }

    #[test]
    fn fk_on_preexisting_column_is_not_flagged() {
        // No column is added in this statement → parent_id is pre-existing → out of scope.
        assert!(flagged(
            "ALTER TABLE child ADD CONSTRAINT fk FOREIGN KEY (parent_id) REFERENCES parent (id)"
        )
        .is_empty());
    }

    #[test]
    fn inline_unique_or_pk_covers_the_fk_column() {
        assert!(
            flagged("CREATE TABLE child (parent_id bigint PRIMARY KEY REFERENCES parent)")
                .is_empty()
        );
        assert!(
            flagged("CREATE TABLE child (parent_id bigint UNIQUE REFERENCES parent)").is_empty()
        );
    }

    #[test]
    fn multi_column_fk_keys_on_the_first_column() {
        let f = flagged(
            "CREATE TABLE child (a bigint, b bigint, FOREIGN KEY (a, b) REFERENCES parent (x, y))",
        );
        assert_eq!(f, vec![(0, "child".to_string(), "a".to_string())]);
        // An index leading with the first column covers it.
        assert!(flagged(
            "CREATE TABLE child (a bigint, b bigint, FOREIGN KEY (a, b) REFERENCES parent (x, y)); \
             CREATE INDEX ON child (a, b)"
        )
        .is_empty());
    }

    #[test]
    fn table_level_pk_first_key_covers_the_fk_column() {
        assert!(flagged(
            "CREATE TABLE child (parent_id bigint REFERENCES parent, PRIMARY KEY (parent_id))"
        )
        .is_empty());
    }

    #[test]
    fn lint_sql_emits_a_warning_for_a_new_column_fk() {
        use crate::{lint_sql, LintOptions, Severity};
        let f = lint_sql(
            "ALTER TABLE child ADD COLUMN parent_id bigint REFERENCES parent",
            &LintOptions::default(),
        )
        .unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "fk-without-covering-index")
            .expect("rule must fire through the engine");
        assert_eq!(hit.severity, Severity::Warning);
    }

    #[test]
    fn fk_finding_is_inline_suppressible() {
        use crate::{lint_sql, LintOptions};
        // CREATE TABLE form avoids require-timeout so the FK finding is the only one.
        let sql = "-- pgsafe:ignore fk-without-covering-index index follows in a later migration\n\
                   CREATE TABLE child (parent_id bigint REFERENCES parent)";
        let f = lint_sql(sql, &LintOptions::default()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "fk-without-covering-index")
            .expect("rule must fire");
        assert!(
            hit.is_suppressed(),
            "directive must suppress the FK finding"
        );
    }

    #[test]
    fn disabled_fk_rule_is_silent() {
        use crate::{lint_sql, LintOptions};
        let opts = LintOptions {
            disabled_rules: ["fk-without-covering-index".to_string()]
                .into_iter()
                .collect(),
            ..LintOptions::default()
        };
        let f = lint_sql(
            "CREATE TABLE child (parent_id bigint REFERENCES parent)",
            &opts,
        )
        .unwrap();
        assert!(f.iter().all(|f| f.rule_id != "fk-without-covering-index"));
    }
}
