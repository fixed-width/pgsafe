//! Policy lint (opt-in, off by default): flag a `DO` block. pgsafe analyzes top-level SQL statements,
//! but a `DO $$ … $$` block's body is procedural PL/pgSQL that the SQL parser exposes only as an opaque
//! string — so any DDL or DML the block runs bypasses every rule. Teams that want migrations to be
//! fully analyzable enable this to keep hazards out of unchecked blocks. Engine-synthesized; not a
//! registered `Rule`.

use pg_query::protobuf::RawStmt;
use pg_query::NodeEnum;

pub(crate) const ID: &str = "unchecked-do-block";
pub(crate) const GUIDANCE: &str =
    "Move the DDL/DML out of the DO block into top-level statements so pgsafe can check it, or suppress \
     this finding with an inline `-- pgsafe:ignore unchecked-do-block <reason>` once the block is reviewed.";

/// `(statement_index, message)` for each `DO` block in the migration — its body is opaque to the linter.
pub(crate) fn unchecked_do_blocks(stmts: &[RawStmt]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        if matches!(node, NodeEnum::DoStmt(_)) {
            out.push((
                i,
                "A DO block's procedural body is not analyzed — any DDL or DML inside it bypasses \
                 every pgsafe rule."
                    .to_string(),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::unchecked_do_blocks;
    use crate::{lint_sql, LintOptions};

    fn enabled() -> LintOptions {
        LintOptions {
            enabled_rules: ["unchecked-do-block".to_string()].into_iter().collect(),
            ..LintOptions::default()
        }
    }

    fn flagged(sql: &str) -> usize {
        unchecked_do_blocks(&pg_query::parse(sql).unwrap().protobuf.stmts).len()
    }

    #[test]
    fn do_block_is_flagged() {
        assert_eq!(
            flagged("DO $$ BEGIN ALTER TABLE big ADD COLUMN x int NOT NULL DEFAULT 1; END $$;"),
            1
        );
    }

    #[test]
    fn non_do_statement_is_not_flagged() {
        assert_eq!(flagged("CREATE TABLE t (id int)"), 0);
    }

    #[test]
    fn do_with_explicit_language_clause_is_flagged() {
        // `DO LANGUAGE plpgsql $$ … $$` parses to the same DoStmt node as the positional form.
        assert_eq!(
            flagged("DO LANGUAGE plpgsql $$ BEGIN PERFORM 1; END $$;"),
            1
        );
    }

    #[test]
    fn each_do_block_is_flagged_once() {
        // two DO blocks around a plain statement — one finding each, none for the CREATE.
        let sql = "DO $$ BEGIN PERFORM 1; END $$;\n\
                   CREATE TABLE t (id int);\n\
                   DO $$ BEGIN PERFORM 2; END $$;";
        assert_eq!(flagged(sql), 2);
    }

    #[test]
    fn off_by_default() {
        let f = lint_sql("DO $$ BEGIN PERFORM 1; END $$;", &LintOptions::default()).unwrap();
        assert!(f.iter().all(|f| f.rule_id != "unchecked-do-block"));
    }

    #[test]
    fn fires_when_enabled() {
        use crate::Severity;
        let f = lint_sql("DO $$ BEGIN PERFORM 1; END $$;", &enabled()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "unchecked-do-block")
            .expect("rule must fire when enabled");
        assert_eq!(hit.severity, Severity::Warning);
    }

    #[test]
    fn disabled_overrides_enabled() {
        // a rule both enabled and disabled is silenced — the disabled set wins.
        let opts = LintOptions {
            enabled_rules: ["unchecked-do-block".to_string()].into_iter().collect(),
            disabled_rules: ["unchecked-do-block".to_string()].into_iter().collect(),
            ..LintOptions::default()
        };
        let f = lint_sql("DO $$ BEGIN PERFORM 1; END $$;", &opts).unwrap();
        assert!(f.iter().all(|f| f.rule_id != "unchecked-do-block"));
    }

    #[test]
    fn suppressible_when_enabled() {
        let sql = "-- pgsafe:ignore unchecked-do-block reviewed\nDO $$ BEGIN PERFORM 1; END $$;";
        let f = lint_sql(sql, &enabled()).unwrap();
        let hit = f
            .iter()
            .find(|f| f.rule_id == "unchecked-do-block")
            .expect("rule must fire");
        assert!(hit.is_suppressed());
    }
}
