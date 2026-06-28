//! Policy lint (opt-in, off by default): flag DDL that omits IF [NOT] EXISTS — a
//! CREATE TABLE/INDEX/SEQUENCE/SCHEMA without IF NOT EXISTS, or a DROP without IF EXISTS. Idempotent,
//! re-runnable migrations guard their DDL this way. Engine-synthesized; not a registered `Rule`.

use pg_query::protobuf::RawStmt;
use pg_query::NodeEnum;

pub(crate) const ID: &str = "require-if-exists";
pub(crate) const GUIDANCE: &str =
    "Add IF NOT EXISTS (CREATE) or IF EXISTS (DROP) so re-running the migration does not error.";

/// `(statement_index, message)` for each CREATE missing `IF NOT EXISTS` or DROP missing `IF EXISTS`.
pub(crate) fn missing_if_exists(stmts: &[RawStmt]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        let msg = match node {
            NodeEnum::CreateStmt(c) if !c.if_not_exists => {
                Some("CREATE TABLE without IF NOT EXISTS is not idempotent — it errors if the table already exists.")
            }
            NodeEnum::IndexStmt(idx) if !idx.if_not_exists => {
                Some("CREATE INDEX without IF NOT EXISTS is not idempotent — it errors if the index already exists.")
            }
            NodeEnum::CreateSeqStmt(s) if !s.if_not_exists => {
                Some("CREATE SEQUENCE without IF NOT EXISTS is not idempotent — it errors if the sequence already exists.")
            }
            NodeEnum::CreateSchemaStmt(s) if !s.if_not_exists => {
                Some("CREATE SCHEMA without IF NOT EXISTS is not idempotent — it errors if the schema already exists.")
            }
            NodeEnum::DropStmt(d) if !d.missing_ok => {
                Some("DROP without IF EXISTS is not idempotent — it errors if the object does not exist.")
            }
            _ => None,
        };
        if let Some(m) = msg {
            out.push((i, m.to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::missing_if_exists;
    use crate::{lint_sql, LintOptions};

    fn enabled() -> LintOptions {
        LintOptions {
            enabled_rules: ["require-if-exists".to_string()].into_iter().collect(),
            ..LintOptions::default()
        }
    }

    fn flagged(sql: &str) -> usize {
        missing_if_exists(&pg_query::parse(sql).unwrap().protobuf.stmts).len()
    }

    #[test]
    fn create_table_without_guard_is_flagged() {
        assert_eq!(flagged("CREATE TABLE t (id int)"), 1);
    }

    #[test]
    fn create_table_with_guard_is_not_flagged() {
        assert_eq!(flagged("CREATE TABLE IF NOT EXISTS t (id int)"), 0);
    }

    #[test]
    fn create_index_sequence_schema_without_guard_are_flagged() {
        assert_eq!(flagged("CREATE INDEX i ON t (x)"), 1);
        assert_eq!(flagged("CREATE SEQUENCE s"), 1);
        assert_eq!(flagged("CREATE SCHEMA app"), 1);
    }

    #[test]
    fn drop_without_if_exists_is_flagged() {
        assert_eq!(flagged("DROP TABLE t"), 1);
    }

    #[test]
    fn drop_with_if_exists_is_not_flagged() {
        assert_eq!(flagged("DROP TABLE IF EXISTS t"), 0);
    }

    #[test]
    fn off_by_default() {
        let f = lint_sql("CREATE TABLE t (id int)", &LintOptions::default()).unwrap();
        assert!(f.iter().all(|f| f.rule_id != "require-if-exists"));
    }

    #[test]
    fn fires_when_enabled() {
        use crate::Severity;
        let f = lint_sql("CREATE TABLE t (id int)", &enabled()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "require-if-exists")
            .expect("rule must fire when enabled");
        assert_eq!(hit.severity, Severity::Warning);
    }

    #[test]
    fn suppressible_when_enabled() {
        let sql = "-- pgsafe:ignore require-if-exists bootstrap\nCREATE TABLE t (id int)";
        let f = lint_sql(sql, &enabled()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "require-if-exists")
            .expect("rule must fire");
        assert!(hit.is_suppressed());
    }
}
