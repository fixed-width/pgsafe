//! Policy lint (opt-in, off by default): flag DDL that omits IF [NOT] EXISTS — a
//! CREATE TABLE/INDEX/SEQUENCE/SCHEMA/MATERIALIZED VIEW/TABLE-AS without IF NOT EXISTS, or a DROP
//! without IF EXISTS. Idempotent, re-runnable migrations guard their DDL this way. Engine-synthesized;
//! not a registered `Rule`.

use crate::ast::protobuf::{ObjectType, RawStmt};
use crate::ast::NodeEnum;

use crate::fix::{FixAnchor, FixDraft, FixDraftEdit};

pub(crate) const ID: &str = "require-if-exists";
pub(crate) const GUIDANCE: &str =
    "Add IF NOT EXISTS (CREATE) or IF EXISTS (DROP) so re-running the migration does not error.";

/// `(statement_index, message, fix_draft)` for each CREATE missing `IF NOT EXISTS` or DROP
/// missing `IF EXISTS`. The `fix_draft` is `None` only for DROP forms with an object type that
/// does not have a well-known keyword to anchor on (e.g. `DROP TRIGGER`, `DROP FUNCTION`).
pub(crate) fn missing_if_exists(stmts: &[RawStmt]) -> Vec<(usize, String, Option<FixDraft>)> {
    let mut out = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        let hit: Option<(&str, Option<FixDraft>)> = match node {
            NodeEnum::CreateStmt(c) if !c.if_not_exists => Some((
                "CREATE TABLE without IF NOT EXISTS is not idempotent — it errors if the table already exists.",
                Some(if_not_exists_draft("TABLE")),
            )),
            NodeEnum::IndexStmt(idx) if !idx.if_not_exists => {
                // Anchor after CONCURRENTLY when present so the clause lands in the right spot.
                let anchor_kw = if idx.concurrent { "CONCURRENTLY" } else { "INDEX" };
                // IF NOT EXISTS requires a named index; `CREATE INDEX ON t (x)` (unnamed) does
                // not accept IF NOT EXISTS — inserting it produces a parse error.
                let fix = if !idx.idxname.is_empty() {
                    Some(if_not_exists_draft(anchor_kw))
                } else {
                    None
                };
                Some((
                    "CREATE INDEX without IF NOT EXISTS is not idempotent — it errors if the index already exists.",
                    fix,
                ))
            }
            NodeEnum::CreateSeqStmt(s) if !s.if_not_exists => Some((
                "CREATE SEQUENCE without IF NOT EXISTS is not idempotent — it errors if the sequence already exists.",
                Some(if_not_exists_draft("SEQUENCE")),
            )),
            NodeEnum::CreateSchemaStmt(s) if !s.if_not_exists => Some((
                "CREATE SCHEMA without IF NOT EXISTS is not idempotent — it errors if the schema already exists.",
                Some(if_not_exists_draft("SCHEMA")),
            )),
            // CREATE MATERIALIZED VIEW and CREATE TABLE … AS both parse as CreateTableAsStmt and both
            // accept IF NOT EXISTS; the objtype distinguishes them. SELECT INTO can lower to this node
            // too but has no IF NOT EXISTS syntax, so `is_select_into` is excluded.
            NodeEnum::CreateTableAsStmt(c) if !c.if_not_exists && !c.is_select_into => {
                match ObjectType::try_from(c.objtype) {
                    Ok(ObjectType::ObjectMatview) => Some((
                        "CREATE MATERIALIZED VIEW without IF NOT EXISTS is not idempotent — it errors if the materialized view already exists.",
                        Some(if_not_exists_draft("VIEW")),
                    )),
                    Ok(ObjectType::ObjectTable) => Some((
                        "CREATE TABLE AS without IF NOT EXISTS is not idempotent — it errors if the table already exists.",
                        Some(if_not_exists_draft("TABLE")),
                    )),
                    // Any other objtype in a CreateTableAsStmt is unexpected (the parser only emits
                    // ObjectTable or ObjectMatview here). Emit nothing rather than a possibly
                    // unfixable "add IF NOT EXISTS" finding for an unrecognized form.
                    Ok(_) | Err(_) => None,
                }
            }
            NodeEnum::DropStmt(d) if !d.missing_ok => Some((
                "DROP without IF EXISTS is not idempotent — it errors if the object does not exist.",
                drop_if_exists_draft(d.remove_type, d.concurrent),
            )),
            _ => None,
        };
        if let Some((msg, draft)) = hit {
            out.push((i, msg.to_string(), draft));
        }
    }
    out
}

/// Build a `FixDraft` that inserts ` IF NOT EXISTS` after `anchor_kw`.
fn if_not_exists_draft(anchor_kw: &'static str) -> FixDraft {
    FixDraft {
        title: "Add IF NOT EXISTS",
        edits: vec![FixDraftEdit {
            anchor: FixAnchor::AfterKeyword(anchor_kw),
            replacement: " IF NOT EXISTS".into(),
        }],
    }
}

/// Build a `FixDraft` that inserts ` IF EXISTS` after the appropriate keyword for the given DROP
/// `remove_type`. Returns `None` for object types whose syntax does not have a single
/// well-known keyword anchor (e.g. `DROP TRIGGER`, `DROP FUNCTION`).
fn drop_if_exists_draft(remove_type: i32, concurrent: bool) -> Option<FixDraft> {
    let anchor_kw: &'static str = match ObjectType::try_from(remove_type) {
        Ok(ObjectType::ObjectTable) => "TABLE",
        // DROP INDEX CONCURRENTLY — insert after CONCURRENTLY to get `DROP INDEX CONCURRENTLY IF EXISTS`.
        Ok(ObjectType::ObjectIndex) => {
            if concurrent {
                "CONCURRENTLY"
            } else {
                "INDEX"
            }
        }
        Ok(ObjectType::ObjectSequence) => "SEQUENCE",
        Ok(ObjectType::ObjectSchema) => "SCHEMA",
        // Both DROP VIEW and DROP MATERIALIZED VIEW anchor after VIEW.
        Ok(ObjectType::ObjectView | ObjectType::ObjectMatview) => "VIEW",
        Ok(ObjectType::ObjectType) => "TYPE",
        // Other object types (TRIGGER, FUNCTION, PROCEDURE, …) do not have a safe single-keyword
        // anchor we can rely on; omit the fix draft rather than misplacing the clause.
        _ => return None,
    };
    Some(FixDraft {
        title: "Add IF EXISTS",
        edits: vec![FixDraftEdit {
            anchor: FixAnchor::AfterKeyword(anchor_kw),
            replacement: " IF EXISTS".into(),
        }],
    })
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
        missing_if_exists(&crate::ast::parse(sql).unwrap().protobuf.stmts).len()
    }

    fn message(sql: &str) -> String {
        missing_if_exists(&crate::ast::parse(sql).unwrap().protobuf.stmts)
            .into_iter()
            .next()
            .map(|(_, m, _)| m)
            .expect("expected a finding")
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
    fn create_index_without_guard_is_flagged() {
        assert_eq!(flagged("CREATE INDEX i ON t (x)"), 1);
    }

    #[test]
    fn create_sequence_without_guard_is_flagged() {
        assert_eq!(flagged("CREATE SEQUENCE s"), 1);
    }

    #[test]
    fn create_schema_without_guard_is_flagged() {
        assert_eq!(flagged("CREATE SCHEMA app"), 1);
    }

    #[test]
    fn create_materialized_view_without_guard_is_flagged() {
        assert_eq!(flagged("CREATE MATERIALIZED VIEW m AS SELECT 1"), 1);
        assert!(message("CREATE MATERIALIZED VIEW m AS SELECT 1").contains("MATERIALIZED VIEW"));
    }

    #[test]
    fn create_materialized_view_with_guard_is_not_flagged() {
        assert_eq!(
            flagged("CREATE MATERIALIZED VIEW IF NOT EXISTS m AS SELECT 1"),
            0
        );
    }

    #[test]
    fn create_table_as_without_guard_is_flagged() {
        assert_eq!(flagged("CREATE TABLE t AS SELECT 1"), 1);
        assert!(message("CREATE TABLE t AS SELECT 1").contains("CREATE TABLE AS"));
    }

    #[test]
    fn create_table_as_with_guard_is_not_flagged() {
        assert_eq!(flagged("CREATE TABLE IF NOT EXISTS t AS SELECT 1"), 0);
    }

    #[test]
    fn select_into_is_not_flagged() {
        // SELECT … INTO has no IF NOT EXISTS syntax, so it must not be flagged (would be unfixable).
        assert_eq!(flagged("SELECT 1 INTO t"), 0);
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
    fn create_materialized_view_fires_through_lint_sql() {
        // end-to-end through the engine, not just the helper: the new form reaches Finding output.
        let f = lint_sql("CREATE MATERIALIZED VIEW m AS SELECT 1", &enabled()).unwrap();
        assert!(f
            .iter()
            .any(|f| f.rule_id == "require-if-exists" && f.message.contains("MATERIALIZED VIEW")));
    }

    #[test]
    fn create_table_as_fires_through_lint_sql() {
        let f = lint_sql("CREATE TABLE t AS SELECT 1", &enabled()).unwrap();
        assert!(f
            .iter()
            .any(|f| f.rule_id == "require-if-exists" && f.message.contains("CREATE TABLE AS")));
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

    // --- fix tests (apply→re-lint pattern) ---

    #[test]
    fn create_table_fix_inserts_if_not_exists() {
        use crate::fix::apply;
        let sql = "CREATE TABLE t (id int)";
        let fs = lint_sql(sql, &enabled()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "require-if-exists")
            .expect("finding present");
        let fix = f.fix.as_ref().expect("fix present");
        assert_eq!(fix.title, "Add IF NOT EXISTS");
        let fixed = apply(sql, &fix.edits);
        assert_eq!(fixed, "CREATE TABLE IF NOT EXISTS t (id int)");
        assert!(lint_sql(&fixed, &enabled())
            .unwrap()
            .iter()
            .all(|f| f.rule_id != "require-if-exists"));
    }

    #[test]
    fn create_index_concurrent_fix_anchors_after_concurrently() {
        use crate::fix::apply;
        let sql = "CREATE INDEX CONCURRENTLY i ON t (x)";
        let fs = lint_sql(sql, &enabled()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "require-if-exists")
            .expect("finding present");
        let fix = f.fix.as_ref().expect("fix present");
        let fixed = apply(sql, &fix.edits);
        assert_eq!(fixed, "CREATE INDEX CONCURRENTLY IF NOT EXISTS i ON t (x)");
        assert!(lint_sql(&fixed, &enabled())
            .unwrap()
            .iter()
            .all(|f| f.rule_id != "require-if-exists"));
    }

    #[test]
    fn drop_table_fix_inserts_if_exists() {
        use crate::fix::apply;
        let sql = "DROP TABLE t";
        let fs = lint_sql(sql, &enabled()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "require-if-exists")
            .expect("finding present");
        let fix = f.fix.as_ref().expect("fix present");
        assert_eq!(fix.title, "Add IF EXISTS");
        let fixed = apply(sql, &fix.edits);
        assert_eq!(fixed, "DROP TABLE IF EXISTS t");
        assert!(lint_sql(&fixed, &enabled())
            .unwrap()
            .iter()
            .all(|f| f.rule_id != "require-if-exists"));
    }

    #[test]
    fn create_materialized_view_fix_inserts_if_not_exists() {
        use crate::fix::apply;
        let sql = "CREATE MATERIALIZED VIEW m AS SELECT 1";
        let fs = lint_sql(sql, &enabled()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "require-if-exists")
            .expect("finding present");
        let fix = f.fix.as_ref().expect("fix present");
        let fixed = apply(sql, &fix.edits);
        assert_eq!(
            fixed,
            "CREATE MATERIALIZED VIEW IF NOT EXISTS m AS SELECT 1"
        );
        assert!(lint_sql(&fixed, &enabled())
            .unwrap()
            .iter()
            .all(|f| f.rule_id != "require-if-exists"));
    }

    #[test]
    fn drop_function_unmapped_form_has_no_fix() {
        // DROP FUNCTION is flagged (missing IF EXISTS) but has no fix draft — the object type
        // doesn't have a safe single-keyword anchor in the DROP syntax.
        let fs = lint_sql("DROP FUNCTION foo()", &enabled()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "require-if-exists")
            .expect("finding must fire for unmapped DROP");
        assert!(
            f.fix.is_none(),
            "unmapped DROP form must not emit a fix draft"
        );
    }

    #[test]
    fn unnamed_create_index_finding_fires_but_fix_is_none() {
        // `CREATE INDEX ON t (x)` — unnamed, valid PG syntax, but IF NOT EXISTS requires a
        // name. The finding must still fire; only the fix is suppressed.
        let fs = lint_sql("CREATE INDEX ON t (x)", &enabled()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "require-if-exists")
            .expect("finding must fire for unnamed CREATE INDEX");
        assert!(
            f.fix.is_none(),
            "unnamed CREATE INDEX must not emit a fix draft"
        );
    }

    #[test]
    fn drop_index_concurrently_fix_inserts_if_exists_after_concurrently() {
        // `DROP INDEX CONCURRENTLY idx` — IF EXISTS must land after CONCURRENTLY.
        use crate::fix::apply;
        let sql = "DROP INDEX CONCURRENTLY idx";
        let fs = lint_sql(sql, &enabled()).unwrap();
        let f = fs
            .iter()
            .find(|f| f.rule_id == "require-if-exists")
            .expect("finding present");
        let fix = f.fix.as_ref().expect("fix present");
        let fixed = apply(sql, &fix.edits);
        assert_eq!(fixed, "DROP INDEX CONCURRENTLY IF EXISTS idx");
        assert!(lint_sql(&fixed, &enabled())
            .unwrap()
            .iter()
            .all(|f| f.rule_id != "require-if-exists"));
    }
}
