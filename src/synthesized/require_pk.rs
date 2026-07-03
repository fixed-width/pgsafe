//! Policy lint (opt-in, off by default): flag a `CREATE TABLE` that the migration leaves without a
//! primary key. Cross-statement — a PK added by a later `ALTER TABLE … ADD PRIMARY KEY` in the same
//! migration counts. Engine-synthesized, gated on being enabled in the config; not a registered `Rule`.

use std::collections::BTreeSet;

use pg_query::protobuf::{ConstrType, RawStmt};
use pg_query::NodeEnum;

use super::newtable::{lintable_create_relation, rangevar_key};
use crate::rules::{column_has_constraint, defined_columns, defined_table_constraints};

pub(crate) const ID: &str = "require-primary-key";
pub(crate) const MESSAGE: &str =
    "This table is created without a primary key. Logical replication needs one (a table with no \
    replica identity rejects UPDATE/DELETE), and many ORMs and tools assume every table has one.";
pub(crate) const GUIDANCE: &str =
    "Add a primary key — inline (PRIMARY KEY on a column, or a table-level PRIMARY KEY (...)) or in a \
    later ALTER TABLE ... ADD PRIMARY KEY in the same migration.";

/// The table a `CREATE TABLE` / `ALTER TABLE` operates on.
fn table_key(node: &NodeEnum) -> Option<String> {
    let rv = match node {
        NodeEnum::CreateStmt(c) => c.relation.as_ref(),
        NodeEnum::AlterTableStmt(a) => a.relation.as_ref(),
        _ => None,
    }?;
    Some(rangevar_key(rv))
}

/// Whether a `CREATE`/`ALTER` node introduces a primary key on its table — an inline column
/// `PRIMARY KEY`, a table-level `PRIMARY KEY`, an `ADD CONSTRAINT … PRIMARY KEY`, or an
/// `ADD COLUMN … PRIMARY KEY`.
fn introduces_primary_key(node: &NodeEnum) -> bool {
    defined_columns(node)
        .iter()
        .any(|c| column_has_constraint(c, ConstrType::ConstrPrimary))
        || defined_table_constraints(node)
            .iter()
            .any(|c| ConstrType::try_from(c.contype) == Ok(ConstrType::ConstrPrimary))
}

/// The table key of a `CREATE TABLE` that needs a primary key under this policy: a persistent
/// (non-temp) table that is not a partition child (`PARTITION OF`). `None` otherwise.
fn create_needing_pk(node: &NodeEnum) -> Option<String> {
    lintable_create_relation(node).map(rangevar_key)
}

/// Indices of `CREATE TABLE` statements the migration leaves without a primary key.
pub(crate) fn tables_without_primary_key(stmts: &[RawStmt]) -> Vec<usize> {
    let mut needs: Vec<(String, usize)> = Vec::new();
    let mut has_pk: BTreeSet<String> = BTreeSet::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        if introduces_primary_key(node) {
            if let Some(key) = table_key(node) {
                has_pk.insert(key);
            }
        }
        if let Some(key) = create_needing_pk(node) {
            needs.push((key, i));
        }
    }
    needs
        .into_iter()
        .filter(|(table, _)| !has_pk.contains(table))
        .map(|(_, i)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::tables_without_primary_key;
    use crate::{lint_sql, LintOptions};

    fn enabled_opts() -> LintOptions {
        LintOptions {
            enabled_rules: ["require-primary-key".to_string()].into_iter().collect(),
            ..LintOptions::default()
        }
    }

    fn flagged(sql: &str) -> Vec<usize> {
        tables_without_primary_key(&pg_query::parse(sql).unwrap().protobuf.stmts)
    }

    #[test]
    fn create_without_pk_is_flagged() {
        assert_eq!(flagged("CREATE TABLE t (id int)"), vec![0]);
    }

    #[test]
    fn inline_column_pk_is_not_flagged() {
        assert!(flagged("CREATE TABLE t (id int PRIMARY KEY)").is_empty());
    }

    #[test]
    fn table_level_pk_is_not_flagged() {
        assert!(flagged("CREATE TABLE t (id int, PRIMARY KEY (id))").is_empty());
    }

    #[test]
    fn pk_added_by_later_alter_is_not_flagged() {
        assert!(flagged("CREATE TABLE t (id int); ALTER TABLE t ADD PRIMARY KEY (id);").is_empty());
        assert!(flagged(
            "CREATE TABLE t (id int); ALTER TABLE t ADD CONSTRAINT pk PRIMARY KEY (id);"
        )
        .is_empty());
        // a PK added via ALTER ... ADD COLUMN ... PRIMARY KEY also counts
        assert!(
            flagged("CREATE TABLE t (a int); ALTER TABLE t ADD COLUMN id int PRIMARY KEY;")
                .is_empty()
        );
    }

    #[test]
    fn temp_table_is_not_flagged() {
        assert!(flagged("CREATE TEMP TABLE t (id int)").is_empty());
    }

    #[test]
    fn partition_child_is_not_flagged() {
        assert!(flagged("CREATE TABLE c PARTITION OF p FOR VALUES FROM (0) TO (100)").is_empty());
    }

    #[test]
    fn off_by_default() {
        let f = lint_sql("CREATE TABLE t (id int)", &LintOptions::default()).unwrap();
        assert!(f.iter().all(|f| f.rule_id != "require-primary-key"));
    }

    #[test]
    fn fires_when_enabled() {
        use crate::Severity;
        let f = lint_sql("CREATE TABLE t (id int)", &enabled_opts()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "require-primary-key")
            .expect("rule must fire when enabled");
        assert_eq!(hit.severity, Severity::Warning);
    }

    #[test]
    fn inline_suppressible_when_enabled() {
        let sql = "-- pgsafe:ignore require-primary-key lookup table, no PK by design\n\
                   CREATE TABLE t (id int)";
        let f = lint_sql(sql, &enabled_opts()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "require-primary-key")
            .expect("rule must fire");
        assert!(hit.is_suppressed());
    }
}
