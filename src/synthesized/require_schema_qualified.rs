//! Policy lint (opt-in, off by default): flag a DDL statement whose **target** relation is named
//! without a schema qualifier — it resolves through `search_path`, which is environment-dependent
//! and a migration footgun.
//!
//! Covered targets (the relation a statement creates or operates on): `CREATE TABLE`,
//! `CREATE TABLE … AS` / `SELECT … INTO` / `CREATE MATERIALIZED VIEW`, `CREATE VIEW`,
//! `CREATE SEQUENCE`, `CREATE FOREIGN TABLE`, `ALTER TABLE`, `CREATE INDEX`, `CREATE TRIGGER`
//! (its `ON` table), `ALTER … RENAME`, `ALTER … SET SCHEMA`, `TRUNCATE`, and `DROP` of a relation
//! (table/index/view/materialized view/sequence/foreign table). Names are reported relkind-neutral
//! ("Unqualified name `x`") because one node kind (`AlterTableStmt`, `RenameStmt`, `DropStmt`) spans
//! tables, indexes, and views alike.
//!
//! Intentionally out of scope: non-relation objects (e.g. `DROP FUNCTION`, `ALTER TYPE … RENAME`),
//! whose names resolve through `search_path` too but are a separate concern; and a few rare
//! sequence/trigger corners — `ALTER SEQUENCE … RESTART`-style ops (`AlterSeqStmt`) and a
//! constraint trigger's `FROM` referenced table (`CreateTrigStmt.constrrel`).
//!
//! Temp exemption: a `CREATE TEMP` target (parse-time `relpersistence == "t"`) resolves in `pg_temp`
//! and is exempt. A later bare reference to that temp (e.g. `TRUNCATE t`) carries `relpersistence ==
//! "p"` at parse time — the parser can't know it is temp — so it may still be flagged (suppressible).
//!
//! Engine-synthesized, gated on being enabled in the config; not a registered `Rule`.

use crate::ast::protobuf::{ObjectType, RangeVar, RawStmt};
use crate::ast::NodeEnum;

use super::newtable::RELPERSISTENCE_TEMP;

pub(crate) const ID: &str = "require-schema-qualified";
pub(crate) const GUIDANCE: &str =
    "Qualify the name with its schema (e.g. `public.<name>`) so resolution does not depend on the \
    session's search_path.";

/// Whether an object type is a relation whose name resolves through `search_path` (so a `DROP`
/// or rename of it can hit the wrong schema).
fn is_relation_objtype(objtype: i32) -> bool {
    matches!(
        ObjectType::try_from(objtype),
        Ok(ObjectType::ObjectTable
            | ObjectType::ObjectIndex
            | ObjectType::ObjectView
            | ObjectType::ObjectMatview
            | ObjectType::ObjectSequence
            | ObjectType::ObjectForeignTable)
    )
}

/// DDL target relations carried as a `RangeVar` (empty for nodes without one).
fn target_rangevars(node: &NodeEnum) -> Vec<&RangeVar> {
    match node {
        NodeEnum::CreateStmt(c) => c.relation.as_ref().into_iter().collect(),
        NodeEnum::CreateTableAsStmt(c) => c
            .into
            .as_ref()
            .and_then(|i| i.rel.as_ref())
            .into_iter()
            .collect(),
        // Legacy `SELECT … INTO newtab` (the pre-CTAS spelling) parses as a SelectStmt with an
        // into_clause; a plain SELECT has none, so this arm is empty for ordinary queries.
        NodeEnum::SelectStmt(s) => s
            .into_clause
            .as_ref()
            .and_then(|i| i.rel.as_ref())
            .into_iter()
            .collect(),
        NodeEnum::ViewStmt(v) => v.view.as_ref().into_iter().collect(),
        NodeEnum::CreateSeqStmt(s) => s.sequence.as_ref().into_iter().collect(),
        NodeEnum::CreateForeignTableStmt(f) => f
            .base_stmt
            .as_ref()
            .and_then(|b| b.relation.as_ref())
            .into_iter()
            .collect(),
        NodeEnum::AlterTableStmt(a) => a.relation.as_ref().into_iter().collect(),
        NodeEnum::IndexStmt(i) => i.relation.as_ref().into_iter().collect(),
        NodeEnum::CreateTrigStmt(t) => t.relation.as_ref().into_iter().collect(),
        // `relation` is `Some` only for relation-targeted renames/schema-moves (a table/index/view,
        // incl. RENAME COLUMN/CONSTRAINT); non-relation forms (RENAME on a type/function) leave it None.
        NodeEnum::RenameStmt(r) => r.relation.as_ref().into_iter().collect(),
        NodeEnum::AlterObjectSchemaStmt(a) => a.relation.as_ref().into_iter().collect(),
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

/// The unqualified single-part relation names a `DROP` targets. `DROP` names are `List`s of name
/// parts, not `RangeVar`s (so they need separate extraction); a one-part name (`["t"]`) is
/// unqualified, a two-part name (`["schema", "t"]`) is qualified.
fn drop_unqualified_names(node: &NodeEnum) -> Vec<String> {
    let NodeEnum::DropStmt(d) = node else {
        return Vec::new();
    };
    if !is_relation_objtype(d.remove_type) {
        return Vec::new();
    }
    d.objects
        .iter()
        .filter_map(|obj| {
            let NodeEnum::List(list) = obj.node.as_ref()? else {
                return None;
            };
            let parts: Vec<&str> = list
                .items
                .iter()
                .filter_map(|n| match n.node.as_ref() {
                    Some(NodeEnum::String(s)) => Some(s.sval.as_str()),
                    _ => None,
                })
                .collect();
            match parts.as_slice() {
                [relname] if !relname.is_empty() => Some((*relname).to_string()),
                _ => None,
            }
        })
        .collect()
}

/// `(statement_index, relation_name)` for every DDL target whose name is unqualified (empty
/// `schemaname`, or a one-part `DROP` name). The `String` is the raw relation name — the caller
/// formats it into the finding message. `CREATE TEMP` targets are exempt.
pub(crate) fn unqualified_targets(stmts: &[RawStmt]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, raw) in stmts.iter().enumerate() {
        let Some(node) = raw.stmt.as_ref().and_then(|b| b.node.as_ref()) else {
            continue;
        };
        for rv in target_rangevars(node) {
            if rv.relpersistence == RELPERSISTENCE_TEMP {
                continue;
            }
            if rv.schemaname.is_empty() && !rv.relname.is_empty() {
                out.push((i, rv.relname.clone()));
            }
        }
        for name in drop_unqualified_names(node) {
            out.push((i, name));
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
    fn names(sql: &str) -> Vec<String> {
        flagged(sql).into_iter().map(|(_, n)| n).collect()
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
    fn flags_create_table_as_and_matview() {
        assert_eq!(names("CREATE TABLE foo AS SELECT 1"), vec!["foo"]);
        assert_eq!(names("CREATE MATERIALIZED VIEW mv AS SELECT 1"), vec!["mv"]);
    }

    #[test]
    fn flags_create_view_sequence_foreign_table() {
        assert_eq!(names("CREATE VIEW v AS SELECT 1"), vec!["v"]);
        assert_eq!(names("CREATE SEQUENCE s"), vec!["s"]);
        assert_eq!(names("SELECT 1 INTO newtab"), vec!["newtab"]);
        assert_eq!(
            names("CREATE FOREIGN TABLE ft (id int) SERVER srv"),
            vec!["ft"]
        );
        // qualified + temp forms are exempt
        assert!(flagged("CREATE VIEW public.v AS SELECT 1").is_empty());
        assert!(flagged("CREATE TEMP VIEW v AS SELECT 1").is_empty());
        assert!(flagged("CREATE SEQUENCE app.s").is_empty());
    }

    #[test]
    fn flags_rename_and_set_schema() {
        assert_eq!(
            names("ALTER TABLE orders RENAME TO orders2"),
            vec!["orders"]
        );
        assert_eq!(
            names("ALTER TABLE orders RENAME COLUMN a TO b"),
            vec!["orders"]
        );
        assert_eq!(names("ALTER INDEX i RENAME TO j"), vec!["i"]);
        assert_eq!(names("ALTER TABLE orders SET SCHEMA app"), vec!["orders"]);
    }

    #[test]
    fn flags_index_trigger_and_truncate() {
        assert_eq!(names("CREATE INDEX i ON t (x)"), vec!["t"]);
        assert_eq!(
            names("CREATE TRIGGER trg AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()"),
            vec!["t"]
        );
        assert_eq!(names("TRUNCATE t"), vec!["t"]);
        assert_eq!(names("TRUNCATE a, public.b"), vec!["a"]);
    }

    #[test]
    fn flags_unqualified_drop_relations() {
        assert_eq!(names("DROP TABLE t"), vec!["t"]);
        assert_eq!(names("DROP INDEX i"), vec!["i"]);
        assert_eq!(names("DROP VIEW v"), vec!["v"]);
    }

    #[test]
    fn ignores_qualified_targets() {
        assert!(flagged("CREATE TABLE public.t (id int)").is_empty());
        assert!(flagged("ALTER TABLE app.orders ADD COLUMN x int").is_empty());
        assert!(flagged("CREATE INDEX i ON public.t (x)").is_empty());
        assert!(flagged("TRUNCATE public.t").is_empty());
        assert!(flagged("DROP TABLE public.t").is_empty());
        assert!(flagged("ALTER TABLE app.orders RENAME TO orders2").is_empty());
    }

    #[test]
    fn ignores_non_relation_drop() {
        // DROP of a non-relation object (function/type) is out of scope for this rule.
        assert!(flagged("DROP FUNCTION f()").is_empty());
        assert!(flagged("DROP TYPE mytype").is_empty());
    }

    #[test]
    fn ignores_temp_but_flags_unlogged() {
        assert!(flagged("CREATE TEMP TABLE t (id int)").is_empty());
        // UNLOGGED tables live in a real schema, so they still resolve through search_path.
        assert_eq!(names("CREATE UNLOGGED TABLE t (id int)"), vec!["t"]);
    }

    #[test]
    fn attributes_offending_statement_index() {
        // Qualified statement is skipped; the unqualified one is reported at its own index.
        assert_eq!(
            flagged("CREATE TABLE public.a (id int); ALTER TABLE b ADD COLUMN x int"),
            vec![(1, "b".to_string())]
        );
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
        assert!(hit.message.contains("`t`"), "message names the relation");
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
