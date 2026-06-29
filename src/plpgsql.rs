//! Recovery of statically-analyzable SQL from a `DO` block's PL/pgSQL body, via the experimental
//! `pg_query::parse_plpgsql`. Pure: produces the embedded SQL statement texts plus a flag for
//! un-analyzable residue (a dynamic `EXECUTE`, or a body that would not parse). Not a rule — the
//! engine (`lint_sql`) replays the statements through the per-statement rules and uses the residue
//! flag to drive the `unchecked-do-block` rule.
// Items are `pub(crate)` for consumption by Tasks 2 & 3 (not wired yet).
#![allow(dead_code)]

use serde_json::Value;

/// The result of statically analyzing a `DO` block body.
pub(crate) struct DoBodyAnalysis {
    /// SQL statement texts recovered from `PLpgSQL_stmt_execsql` nodes, in body order.
    pub statements: Vec<String>,
    /// `true` if the block holds anything pgsafe cannot statically analyze: a `PLpgSQL_stmt_dynexecute`
    /// (`EXECUTE`), or a body that `parse_plpgsql` rejected.
    pub has_residue: bool,
}

/// Recover the statically-analyzable statements from a `DO` block's source text. A body that does not
/// parse (e.g. a non-PL/pgSQL `DO … LANGUAGE`, or a malformed body) yields no statements and
/// `has_residue = true`.
pub(crate) fn analyze_do_block(do_sql: &str) -> DoBodyAnalysis {
    let Ok(json) = pg_query::parse_plpgsql(do_sql) else {
        return DoBodyAnalysis {
            statements: Vec::new(),
            has_residue: true,
        };
    };
    let mut analysis = DoBodyAnalysis {
        statements: Vec::new(),
        has_residue: false,
    };
    walk(&json, &mut analysis);
    analysis
}

/// Recursively collect every `PLpgSQL_stmt_execsql` query text and flag any `PLpgSQL_stmt_dynexecute`
/// as residue. The plpgsql JSON nests statement objects under control-flow keys (`then_body`, etc.),
/// so a full descent reaches statements inside `IF`/`LOOP`/`CASE`/nested blocks.
fn walk(value: &Value, out: &mut DoBodyAnalysis) {
    match value {
        Value::Object(map) => {
            if let Some(query) = map
                .get("PLpgSQL_stmt_execsql")
                .and_then(|n| n.get("sqlstmt"))
                .and_then(|n| n.get("PLpgSQL_expr"))
                .and_then(|n| n.get("query"))
                .and_then(Value::as_str)
            {
                out.statements.push(query.to_string());
            }
            if map.contains_key("PLpgSQL_stmt_dynexecute") {
                out.has_residue = true;
            }
            for child in map.values() {
                walk(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::analyze_do_block;

    #[test]
    fn recovers_a_single_direct_statement() {
        let a = analyze_do_block("DO $$ BEGIN ALTER TABLE t ADD COLUMN x int; END $$;");
        assert_eq!(a.statements.len(), 1);
        assert!(a.statements[0].contains("ALTER TABLE t"));
        assert!(!a.has_residue);
    }

    #[test]
    fn recovers_multiple_statements_in_body_order() {
        let a = analyze_do_block("DO $$ BEGIN CREATE INDEX i ON t (c); DROP TABLE u; END $$;");
        assert_eq!(a.statements.len(), 2);
        assert!(a.statements[0].contains("CREATE INDEX"));
        assert!(a.statements[1].contains("DROP TABLE"));
        assert!(!a.has_residue);
    }

    #[test]
    fn recovers_statements_nested_in_control_flow() {
        let a =
            analyze_do_block("DO $$ BEGIN IF true THEN CREATE INDEX i ON t (c); END IF; END $$;");
        assert_eq!(a.statements.len(), 1);
        assert!(a.statements[0].contains("CREATE INDEX"));
        assert!(!a.has_residue);
    }

    #[test]
    fn control_only_body_yields_no_statements_and_no_residue() {
        let a = analyze_do_block("DO $$ BEGIN PERFORM 1; END $$;");
        assert!(a.statements.is_empty());
        assert!(!a.has_residue);
    }

    #[test]
    fn dynamic_execute_is_residue() {
        let a = analyze_do_block("DO $$ BEGIN EXECUTE 'ALTER TABLE t ADD COLUMN x int'; END $$;");
        assert!(a.has_residue);
    }

    #[test]
    fn unparsable_body_is_residue_with_no_statements() {
        // missing END IF — parse_plpgsql rejects the body.
        let a = analyze_do_block("DO $$ BEGIN IF true THEN NULL; END $$;");
        assert!(a.has_residue);
        assert!(a.statements.is_empty());
    }

    #[test]
    fn mixed_execsql_and_execute_keeps_static_and_flags_residue() {
        let a = analyze_do_block(
            "DO $$ BEGIN ALTER TABLE t ADD COLUMN x int; EXECUTE 'DROP TABLE u'; END $$;",
        );
        assert_eq!(a.statements.len(), 1);
        assert!(a.statements[0].contains("ALTER TABLE t"));
        assert!(a.has_residue);
    }
}
